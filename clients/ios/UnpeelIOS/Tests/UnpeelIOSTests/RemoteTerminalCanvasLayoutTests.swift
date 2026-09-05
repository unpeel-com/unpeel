import XCTest
@testable import UnpeelIOS

final class RemoteTerminalCanvasLayoutTests: XCTestCase {
    func testTerminalCardUsesStableChromeBoundsInsteadOfCanvasHeight() {
        let screen = CGSize(width: 402, height: 874)
        let resting = RemoteTerminalCanvasLayout.cardFrame(
            in: screen,
            reservedTop: 124,
            reservedBottom: 0
        )
        let keyboardFocused = RemoteTerminalCanvasLayout.cardFrame(
            in: screen,
            reservedTop: 124,
            reservedBottom: 320
        )

        XCTAssertEqual(resting.minY, 124, accuracy: 0.001)
        XCTAssertEqual(resting.maxY, screen.height, accuracy: 0.001)
        XCTAssertEqual(keyboardFocused.minY, resting.minY, accuracy: 0.001)
        XCTAssertEqual(keyboardFocused.maxY, screen.height - 320, accuracy: 0.001)
    }

    func testSidebarRevealLayoutUsesPhoneWidthAndClampsInteractiveTravel() {
        let width = IOSSidebarRevealLayout.drawerWidth(
            for: CGSize(width: 390, height: 844),
            regular: false
        )

        XCTAssertEqual(width, 343.2, accuracy: 0.001)
        XCTAssertEqual(IOSSidebarRevealLayout.revealDistance(
            presented: false,
            interactiveReveal: 120,
            drawerWidth: width
        ), 120, accuracy: 0.001)
        XCTAssertEqual(IOSSidebarRevealLayout.revealDistance(
            presented: false,
            interactiveReveal: 1_000,
            drawerWidth: width
        ), width, accuracy: 0.001)
        XCTAssertEqual(IOSSidebarRevealLayout.revealDistance(
            presented: false,
            interactiveReveal: -20,
            drawerWidth: width
        ), 0, accuracy: 0.001)
    }

    func testSidebarRevealLayoutSettlesFullyOpenOrClosed() {
        let width: CGFloat = 343.2

        XCTAssertEqual(IOSSidebarRevealLayout.revealDistance(
            presented: true,
            interactiveReveal: nil,
            drawerWidth: width
        ), width, accuracy: 0.001)
        XCTAssertEqual(IOSSidebarRevealLayout.revealDistance(
            presented: false,
            interactiveReveal: nil,
            drawerWidth: width
        ), 0, accuracy: 0.001)
    }

    func testSidebarRevealDoesNotClaimTerminalTextSelectionDrag() {
        let start = CGPoint(x: 40, y: 300)
        let rightward = CGSize(width: 120, height: 8)

        XCTAssertTrue(TerminalDetailView.acceptsSidebarReveal(
            startLocation: start,
            translation: rightward,
            screenWidth: 390,
            pointerSelectionActive: false
        ))
        XCTAssertFalse(TerminalDetailView.acceptsSidebarReveal(
            startLocation: start,
            translation: rightward,
            screenWidth: 390,
            pointerSelectionActive: true
        ))
    }

    func testSidebarRevealOnlyStartsWithinLeftFifteenPercent() {
        let rightward = CGSize(width: 120, height: 8)

        XCTAssertTrue(TerminalDetailView.acceptsSidebarReveal(
            startLocation: CGPoint(x: 58.5, y: 300),
            translation: rightward,
            screenWidth: 390,
            pointerSelectionActive: false
        ))
        XCTAssertFalse(TerminalDetailView.acceptsSidebarReveal(
            startLocation: CGPoint(x: 59, y: 300),
            translation: rightward,
            screenWidth: 390,
            pointerSelectionActive: false
        ))
    }

    private let desktopCanvas = CGSize(width: 724, height: 590)

    func testPortraitUsesFixedScaleAndAllowsHorizontalOverflow() {
        let viewport = CGSize(width: 402, height: 874)
        let scale = RemoteTerminalCanvasLayout.baseScale(in: viewport, canvasSize: desktopCanvas)
        let pan = RemoteTerminalCanvasLayout.defaultPan(in: viewport, scale: scale, canvasSize: desktopCanvas)
        let frame = RemoteTerminalCanvasLayout.terminalFrame(
            viewport: viewport,
            canvasSize: desktopCanvas,
            scale: scale,
            pan: pan
        )

        XCTAssertEqual(scale, 1, accuracy: 0.001)
        XCTAssertGreaterThan(frame.width, viewport.width)
        XCTAssertLessThanOrEqual(frame.height, viewport.height + 0.5)
        XCTAssertEqual(frame.minX, 0, accuracy: 0.5)
        XCTAssertEqual(frame.maxY, viewport.height, accuracy: 0.5)
    }

    func testLandscapeUsesFixedScaleAndAnchorsBottomLeft() {
        let viewport = CGSize(width: 874, height: 402)
        let scale = RemoteTerminalCanvasLayout.baseScale(in: viewport, canvasSize: desktopCanvas)
        let pan = RemoteTerminalCanvasLayout.defaultPan(in: viewport, scale: scale, canvasSize: desktopCanvas)
        let frame = RemoteTerminalCanvasLayout.terminalFrame(
            viewport: viewport,
            canvasSize: desktopCanvas,
            scale: scale,
            pan: pan
        )

        XCTAssertEqual(scale, 1, accuracy: 0.001)
        XCTAssertLessThanOrEqual(frame.width, viewport.width)
        XCTAssertGreaterThan(frame.height, viewport.height)
        XCTAssertEqual(frame.minX, 0, accuracy: 0.5)
        XCTAssertEqual(frame.maxY, viewport.height, accuracy: 0.5)
    }

    func testLargeViewportKeepsNaturalTerminalScale() {
        let viewport = CGSize(width: 1024, height: 1366)
        let scale = RemoteTerminalCanvasLayout.baseScale(in: viewport, canvasSize: desktopCanvas)

        XCTAssertEqual(scale, 1, accuracy: 0.001)
    }

    func testRemoteGridKeepsRemoteRowsInsteadOfPhoneDerivedRows() {
        let viewport = CGSize(width: 402, height: 714)
        let canvas = RemoteTerminalCanvasLayout.canvasSize(
            columns: 120,
            rows: 31,
            cellSize: CGSize(width: 8.45, height: 18.65),
            horizontalPadding: 10,
            verticalPadding: 6
        )
        let scale = RemoteTerminalCanvasLayout.baseScale(in: viewport, canvasSize: canvas)
        let frame = RemoteTerminalCanvasLayout.terminalFrame(
            viewport: viewport,
            canvasSize: canvas,
            scale: scale,
            pan: RemoteTerminalCanvasLayout.defaultPan(in: viewport, scale: scale, canvasSize: canvas)
        )

        XCTAssertEqual(canvas.height, CGFloat(31) * 18.65 + 12, accuracy: 0.001)
        XCTAssertEqual(scale, 1, accuracy: 0.001)
        XCTAssertGreaterThan(frame.width, viewport.width)
        XCTAssertLessThanOrEqual(frame.height, viewport.height + 0.5)
        XCTAssertEqual(frame.maxY, viewport.height, accuracy: 0.5)
    }

    func testBottomSlackAnchorsContentNotCanvas() {
        // Canvas built taller than its content (alignment slop + sticky
        // grid-alignment extra): the resting pan must anchor the CONTENT
        // bottom to the viewport bottom, letting the blank slack hang
        // below — not push the top rows up under the title chrome.
        let viewport = CGSize(width: 402, height: 700)
        let slack: CGFloat = 40
        let canvas = CGSize(width: 402, height: 700 + slack)
        let pan = RemoteTerminalCanvasLayout.defaultPan(
            in: viewport, scale: 1, canvasSize: canvas, bottomSlack: slack
        )
        let frame = RemoteTerminalCanvasLayout.terminalFrame(
            viewport: viewport,
            canvasSize: canvas,
            scale: 1,
            pan: pan
        )

        // Content bottom (canvas bottom minus slack) sits on the viewport
        // bottom, so the canvas top no longer overhangs the viewport top.
        XCTAssertEqual(frame.maxY - slack, viewport.height, accuracy: 0.5)
        XCTAssertEqual(frame.minY, 0, accuracy: 0.5)

        // The resting position stays within the pan clamp.
        let clamped = RemoteTerminalCanvasLayout.clamped(
            pan, in: viewport, scale: 1, canvasSize: canvas, bottomSlack: slack
        )
        XCTAssertEqual(clamped.height, pan.height, accuracy: 0.5)

        // Zero slack keeps the original raw bottom-anchoring.
        let rawPan = RemoteTerminalCanvasLayout.defaultPan(
            in: viewport, scale: 1, canvasSize: canvas
        )
        let rawFrame = RemoteTerminalCanvasLayout.terminalFrame(
            viewport: viewport, canvasSize: canvas, scale: 1, pan: rawPan
        )
        XCTAssertEqual(rawFrame.maxY, viewport.height, accuracy: 0.5)
    }

    func testKeyboardShorteningKeepsTerminalTopFixed() {
        let keyboardViewport = CGSize(width: 402, height: 380)
        let canvas = CGSize(width: 402, height: 700)
        let pan = RemoteTerminalCanvasLayout.topAnchoredPan(
            in: keyboardViewport,
            scale: 1,
            canvasSize: canvas
        )
        let frame = RemoteTerminalCanvasLayout.terminalFrame(
            viewport: keyboardViewport,
            canvasSize: canvas,
            scale: 1,
            pan: pan
        )

        XCTAssertEqual(frame.minY, 0, accuracy: 0.5)
        XCTAssertEqual(frame.height, canvas.height, accuracy: 0.5)
        XCTAssertGreaterThan(frame.maxY, keyboardViewport.height)

        let oldBottomAnchoredFrame = RemoteTerminalCanvasLayout.terminalFrame(
            viewport: keyboardViewport,
            canvasSize: canvas,
            scale: 1,
            pan: RemoteTerminalCanvasLayout.defaultPan(
                in: keyboardViewport,
                scale: 1,
                canvasSize: canvas
            )
        )
        XCTAssertLessThan(oldBottomAnchoredFrame.minY, 0)
    }

    func testRowOnlyResizeDoesNotReplayTerminalTail() {
        XCTAssertFalse(RemoteTerminalResizePolicy.needsTailReplay(
            lastReplayedColumns: 44,
            desiredColumns: 44
        ))
        XCTAssertTrue(RemoteTerminalResizePolicy.needsTailReplay(
            lastReplayedColumns: 44,
            desiredColumns: 45
        ))
        XCTAssertTrue(RemoteTerminalResizePolicy.needsTailReplay(
            lastReplayedColumns: 0,
            desiredColumns: 44
        ))
    }

    func testRowOnlyAlignmentRejectsTheTallerPreKeyboardGrid() {
        XCTAssertFalse(RemoteTerminalResizePolicy.rowOnlyAlignmentHasSettled(
            localColumns: 44,
            localRows: 47,
            targetColumns: 44,
            targetRows: 24
        ))
        XCTAssertFalse(RemoteTerminalResizePolicy.rowOnlyAlignmentHasSettled(
            localColumns: 44,
            localRows: 23,
            targetColumns: 44,
            targetRows: 24
        ))
        XCTAssertTrue(RemoteTerminalResizePolicy.rowOnlyAlignmentHasSettled(
            localColumns: 44,
            localRows: 24,
            targetColumns: 44,
            targetRows: 24
        ))
    }

    func testRowOnlyAlignmentIgnoresOnlyTransitionalSameWidthMetrics() {
        XCTAssertTrue(RemoteTerminalResizePolicy.shouldIgnoreObservedGridDuringRowAlignment(
            observedColumns: 44,
            observedRows: 47,
            targetColumns: 44,
            targetRows: 24
        ))
        XCTAssertTrue(RemoteTerminalResizePolicy.shouldIgnoreObservedGridDuringRowAlignment(
            observedColumns: 44,
            observedRows: 23,
            targetColumns: 44,
            targetRows: 24
        ))
        XCTAssertFalse(RemoteTerminalResizePolicy.shouldIgnoreObservedGridDuringRowAlignment(
            observedColumns: 44,
            observedRows: 24,
            targetColumns: 44,
            targetRows: 24
        ))
        XCTAssertFalse(RemoteTerminalResizePolicy.shouldIgnoreObservedGridDuringRowAlignment(
            observedColumns: 45,
            observedRows: 47,
            targetColumns: 44,
            targetRows: 24
        ))
    }

    func testPanClampsToCanvasEdges() {
        let viewport = CGSize(width: 402, height: 874)
        let scale = RemoteTerminalCanvasLayout.baseScale(in: viewport, canvasSize: desktopCanvas)
        let bounds = RemoteTerminalCanvasLayout.panBounds(in: viewport, scale: scale, canvasSize: desktopCanvas)

        let clamped = RemoteTerminalCanvasLayout.clamped(
            CGSize(width: -10_000, height: 10_000),
            in: viewport,
            scale: scale,
            canvasSize: desktopCanvas
        )

        XCTAssertEqual(clamped.width, -bounds.width, accuracy: 0.5)
        XCTAssertEqual(clamped.height, bounds.height, accuracy: 0.5)
    }

    func testMouseModeTrackerDetectsPrivateMouseAndAlternateScreenModes() {
        let tracker = RemoteTerminalMouseModeTracker()

        tracker.feed(Data("\u{1B}[?1049;1000;1006h".utf8))

        XCTAssertTrue(tracker.alternateScreenEnabled)
        XCTAssertTrue(tracker.mouseTrackingEnabled)
    }

    func testMouseModeTrackerHandlesSplitEscapeSequence() {
        let tracker = RemoteTerminalMouseModeTracker()

        tracker.feed(Data("\u{1B}[?10".utf8))
        XCTAssertFalse(tracker.mouseTrackingEnabled)

        tracker.feed(Data("02h".utf8))
        XCTAssertTrue(tracker.mouseTrackingEnabled)
    }

    func testMouseModeTrackerClearsModesOnDisableAndReset() {
        let tracker = RemoteTerminalMouseModeTracker()

        tracker.feed(Data("\u{1B}[?1049;1003h".utf8))
        XCTAssertTrue(tracker.alternateScreenEnabled)
        XCTAssertTrue(tracker.mouseTrackingEnabled)

        tracker.feed(Data("\u{1B}[?1003l".utf8))
        XCTAssertTrue(tracker.alternateScreenEnabled)
        XCTAssertFalse(tracker.mouseTrackingEnabled)
        XCTAssertTrue(tracker.sawMouseOrAlternateDisable)

        tracker.feed(Data("\u{1B}[?1000h".utf8))
        XCTAssertTrue(tracker.mouseTrackingEnabled)
        XCTAssertFalse(tracker.sawMouseOrAlternateDisable)

        tracker.feed(Data("\u{1B}c".utf8))
        XCTAssertFalse(tracker.alternateScreenEnabled)
        XCTAssertFalse(tracker.mouseTrackingEnabled)
        XCTAssertTrue(tracker.sawMouseOrAlternateDisable)
    }

    func testMouseWheelEncoderUsesOneBasedSGRCells() {
        XCTAssertEqual(
            RemoteTerminalMouseEventEncoder.sgrWheelSequence(direction: .down, column: 12, row: 4),
            "\u{1B}[<65;12;4M"
        )
        XCTAssertEqual(
            RemoteTerminalMouseEventEncoder.sgrWheelSequence(direction: .up, column: 0, row: -3, repeats: 2),
            "\u{1B}[<64;1;1M\u{1B}[<64;1;1M"
        )
    }
}

@MainActor
final class MenuPromptDetectorTests: XCTestCase {
    func testMenuLikeInstructionsWithoutChoicesStayWritable() {
        let screen = "Use ↑/↓ to navigate, then press Enter to select."

        XCTAssertFalse(RemoteGhosttyRenderer.viewportHasMenuPrompt(screen))
    }

    func testClaudeSubagentFooterIsPassiveAtPhoneWidth() {
        let screen = """
          ⏺ main           ↑/↓ to select · Enter to
                           view
          ⏺ general-purpose 55m 46s · ↓ 348.3k
        """

        XCTAssertFalse(RemoteGhosttyRenderer.viewportHasMenuPrompt(screen))
    }

    func testClaudeSubagentFooterIsPassiveDuringPartialRepaint() {
        let screen = "  ⏺ main           ↑/↓ to select · Enter to"

        XCTAssertFalse(RemoteGhosttyRenderer.viewportHasMenuPrompt(screen))
    }

    func testQualifiedArrowMenuStillNeedsAttention() {
        let screen = """
          1. Keep working
          2. Stop
          ↑/↓ to select · Enter to confirm · Esc to cancel
        """

        XCTAssertTrue(RemoteGhosttyRenderer.viewportHasMenuPrompt(screen))
    }
}
