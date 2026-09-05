import AppKit
import XCTest
@testable import UnpeelNative

/// The detached session drag's lift rule (`SidebarSessionDragController
/// .liftDecision`) is shared by the dragged-event path AND the 60Hz cursor
/// poll — the poll is what guarantees a lift even when every dragged event
/// after the mouse-down is consumed by a nested event-tracking loop the
/// local monitor never sees. These tests pin the pure decision so the
/// "press never lifts / no card ever appears" regression can't silently
/// return.
@MainActor
final class SidebarSessionDragLiftTests: XCTestCase {
    private let threshold = SidebarSessionDragController.dragThreshold

    func testHoldsInsideSlop() {
        XCTAssertEqual(
            SidebarSessionDragController.liftDecision(
                buttonPressed: true,
                start: NSPoint(x: 100, y: 100),
                current: NSPoint(x: 100, y: 100),
                threshold: threshold
            ),
            .hold
        )
        XCTAssertEqual(
            SidebarSessionDragController.liftDecision(
                buttonPressed: true,
                start: NSPoint(x: 100, y: 100),
                current: NSPoint(x: 103, y: 104),
                threshold: threshold
            ),
            .hold,
            "5pt of travel is inside the 6pt slop"
        )
    }

    func testLiftsAtThresholdInAnyDirection() {
        // Exactly the threshold lifts (>=, not >).
        XCTAssertEqual(
            SidebarSessionDragController.liftDecision(
                buttonPressed: true,
                start: NSPoint(x: 100, y: 100),
                current: NSPoint(x: 100, y: 100 + threshold),
                threshold: threshold
            ),
            .lift
        )
        // Diagonal travel uses euclidean distance, and direction is
        // irrelevant — a leftward/upward drag lifts identically.
        XCTAssertEqual(
            SidebarSessionDragController.liftDecision(
                buttonPressed: true,
                start: NSPoint(x: 100, y: 100),
                current: NSPoint(x: 100 - threshold, y: 100 - threshold),
                threshold: threshold
            ),
            .lift
        )
    }

    func testReleasedButtonCancelsEvenPastThreshold() {
        // The poll sees the button up before any travel-based lift: the
        // press dissolved into a plain click, never a lift.
        XCTAssertEqual(
            SidebarSessionDragController.liftDecision(
                buttonPressed: false,
                start: NSPoint(x: 100, y: 100),
                current: NSPoint(x: 200, y: 200),
                threshold: threshold
            ),
            .cancel
        )
    }

    func testRenameAndRemoveConfirmDoNotArmADrag() {
        XCTAssertTrue(
            SidebarSessionDragController.shouldArmRowDrag(
                rowID: "session",
                editingSessionID: nil,
                confirmingRemoveSessionID: nil
            )
        )
        XCTAssertFalse(
            SidebarSessionDragController.shouldArmRowDrag(
                rowID: "session",
                editingSessionID: "session",
                confirmingRemoveSessionID: nil
            ),
            "drags inside the title field are text selection"
        )
        XCTAssertFalse(
            SidebarSessionDragController.shouldArmRowDrag(
                rowID: "session",
                editingSessionID: nil,
                confirmingRemoveSessionID: "session"
            ),
            "the inline confirm buttons own that click"
        )
        XCTAssertTrue(
            SidebarSessionDragController.shouldArmRowDrag(
                rowID: "session",
                editingSessionID: "other",
                confirmingRemoveSessionID: "other"
            ),
            "a sibling row's rename/confirm does not freeze this row"
        )
    }

    func testVisibleSessionRowDoesNotRequestRevealScroll() {
        let viewport = CGRect(x: 0, y: 100, width: 280, height: 600)
        XCTAssertFalse(
            SidebarSessionDragController.rowNeedsReveal(
                rowFrame: CGRect(x: 8, y: 260, width: 250, height: 28),
                visibleRect: viewport,
                topOcclusion: 52,
                edgeMargin: 48
            )
        )
    }

    func testOccludedAndEdgeSessionRowsRequestRevealScroll() {
        let viewport = CGRect(x: 0, y: 100, width: 280, height: 600)
        XCTAssertTrue(
            SidebarSessionDragController.rowNeedsReveal(
                rowFrame: CGRect(x: 8, y: 130, width: 250, height: 28),
                visibleRect: viewport,
                topOcclusion: 52,
                edgeMargin: 48
            ),
            "a row beneath the titlebar veil still needs revealing"
        )
        XCTAssertTrue(
            SidebarSessionDragController.rowNeedsReveal(
                rowFrame: CGRect(x: 8, y: 640, width: 250, height: 28),
                visibleRect: viewport,
                topOcclusion: 52,
                edgeMargin: 48
            ),
            "a row inside the bottom comfort margin still needs revealing"
        )
    }

    func testRevealMarginClampsForViewportFillingRow() {
        XCTAssertFalse(
            SidebarSessionDragController.rowNeedsReveal(
                rowFrame: CGRect(x: 8, y: 152, width: 250, height: 548),
                visibleRect: CGRect(x: 0, y: 100, width: 280, height: 600),
                topOcclusion: 52,
                edgeMargin: 48
            )
        )
    }

    func testImmediateSelectionUsesBodyButLeavesTrailingControlsAlone() {
        let row = CGRect(x: 10, y: 100, width: 260, height: 28)
        XCTAssertTrue(
            SidebarSessionDragController.immediateSelectionHit(
                point: CGPoint(x: 120, y: 114),
                rowFrame: row,
                trailingControlWidth: 76
            ),
            "the title/body region selects on mouse-down"
        )
        XCTAssertFalse(
            SidebarSessionDragController.immediateSelectionHit(
                point: CGPoint(x: 230, y: 114),
                rowFrame: row,
                trailingControlWidth: 76
            ),
            "hover actions and runtime controls keep their own click"
        )
        XCTAssertFalse(
            SidebarSessionDragController.immediateSelectionHit(
                point: CGPoint(x: 120, y: 90),
                rowFrame: row,
                trailingControlWidth: 76
            ),
            "a neighboring row cannot be selected"
        )
    }

    func testImmediateSelectionKeepsHalfOfNarrowRowClickable() {
        let row = CGRect(x: 0, y: 0, width: 80, height: 28)
        XCTAssertTrue(
            SidebarSessionDragController.immediateSelectionHit(
                point: CGPoint(x: 39, y: 14),
                rowFrame: row,
                trailingControlWidth: 76
            )
        )
        XCTAssertFalse(
            SidebarSessionDragController.immediateSelectionHit(
                point: CGPoint(x: 60, y: 14),
                rowFrame: row,
                trailingControlWidth: 76
            )
        )
    }

    func testConfirmedDragRestoresTheTerminalThatWillReceiveThePane() {
        XCTAssertEqual(
            SidebarSessionDragController.selectionToRestoreForLift(
                draggedID: "dragged",
                selectedBeforePress: "pane-target",
                currentSelection: "dragged"
            ),
            "pane-target"
        )
        XCTAssertNil(
            SidebarSessionDragController.selectionToRestoreForLift(
                draggedID: "dragged",
                selectedBeforePress: "dragged",
                currentSelection: "dragged"
            ),
            "dragging the already-selected Session has no prior pane target"
        )
        XCTAssertNil(
            SidebarSessionDragController.selectionToRestoreForLift(
                draggedID: "dragged",
                selectedBeforePress: "pane-target",
                currentSelection: "third-session"
            ),
            "a newer unrelated selection must not be overwritten at lift"
        )
    }

    /// The transform-gap rule that replaced per-mouse-move tree rebuilds:
    /// rows strictly between the dragged slot and the hovered target shift
    /// one slot toward the origin, opening the gap at the target. Its final
    /// order must match what `previewSessionMove(over: target)` produces in
    /// real layout at drop, or the unanimated handoff would visibly jump.
    func testReorderShiftDirection() {
        func shift(_ row: Int, dragged: Int, target: Int) -> Int {
            SidebarSessionDragController.reorderShiftDirection(
                rowIndex: row, draggedIndex: dragged, targetIndex: target
            )
        }
        // Dragging DOWN (target below origin): rows in (dragged, target]
        // shift up into the vacated slot; the gap opens at the target.
        XCTAssertEqual(shift(2, dragged: 1, target: 3), -1)
        XCTAssertEqual(shift(3, dragged: 1, target: 3), -1)
        XCTAssertEqual(shift(4, dragged: 1, target: 3), 0, "below the gap")
        XCTAssertEqual(shift(0, dragged: 1, target: 3), 0, "above the origin")
        // Dragging UP: rows in [target, dragged) shift down.
        XCTAssertEqual(shift(1, dragged: 3, target: 1), 1)
        XCTAssertEqual(shift(2, dragged: 3, target: 1), 1)
        XCTAssertEqual(shift(0, dragged: 3, target: 1), 0)
        XCTAssertEqual(shift(4, dragged: 3, target: 1), 0)
        // The dragged row's own invisible slot never shifts, and a target
        // at home moves nothing.
        XCTAssertEqual(shift(1, dragged: 1, target: 3), 0)
        XCTAssertEqual(shift(3, dragged: 3, target: 3), 0)
        XCTAssertEqual(shift(2, dragged: 2, target: 2), 0)
    }

    /// Gap-at-target matches previewSessionMove's directional-swap order:
    /// moving down lands the dragged row AFTER the anchor, moving up BEFORE
    /// it — replayed here against the shift rule's visual order.
    func testShiftVisualOrderMatchesPreviewMove() {
        let ids = ["a", "b", "c", "d", "e"]
        func previewMove(draggedID: String, over targetID: String) -> [String] {
            // Mirrors UnpeelStore.previewSessionMove: indexes in the full
            // list, remove dragged, insert at the target's original index.
            var out = ids
            guard let from = out.firstIndex(of: draggedID),
                  let to = out.firstIndex(of: targetID) else { return out }
            out.remove(at: from)
            out.insert(draggedID, at: to)
            return out
        }
        func visualOrder(dragged: Int, target: Int) -> [String] {
            // Rows sorted by their shifted slot; the dragged row occupies
            // the target slot (that is where the gap — and the card — sit).
            var slots: [(id: String, slot: Int)] = []
            for (index, id) in ids.enumerated() where index != dragged {
                let shift = SidebarSessionDragController.reorderShiftDirection(
                    rowIndex: index, draggedIndex: dragged, targetIndex: target
                )
                slots.append((id, index + shift))
            }
            slots.append((ids[dragged], target))
            return slots.sorted { $0.slot < $1.slot }.map(\.id)
        }
        XCTAssertEqual(
            visualOrder(dragged: 1, target: 3),
            previewMove(draggedID: "b", over: "d")
        )
        XCTAssertEqual(
            visualOrder(dragged: 3, target: 1),
            previewMove(draggedID: "d", over: "b")
        )
        XCTAssertEqual(
            visualOrder(dragged: 0, target: 4),
            previewMove(draggedID: "a", over: "e")
        )
        XCTAssertEqual(
            visualOrder(dragged: 4, target: 0),
            previewMove(draggedID: "e", over: "a")
        )
        XCTAssertEqual(
            visualOrder(dragged: 2, target: 2),
            ids,
            "gap at home is the persisted order"
        )
    }

    /// The card's magnetic lean toward a split drop zone must be continuous
    /// in the cursor position: 0 at the zone center (a surviving shift
    /// would flip sign there — a visible jump), 0 at/beyond the attraction
    /// radius (no snap on entry), pulling TOWARD the center in between,
    /// and clamped to the subtle max.
    func testDropZoneMagnetShift() {
        func shift(_ x: CGFloat) -> CGFloat {
            SidebarSessionDragController.dropZoneMagnetShift(
                cursorX: x, zoneCenterX: 300, attractionRadius: 300
            )
        }
        XCTAssertEqual(shift(300), 0, "no lean at the zone center")
        XCTAssertEqual(shift(0), 0, "no lean at the attraction radius")
        XCTAssertEqual(shift(-100), 0, "no lean beyond the radius")
        XCTAssertGreaterThan(shift(150), 0, "left of center pulls right")
        XCTAssertLessThan(shift(450), 0, "right of center pulls left")
        // Symmetric approach, and never past the cap.
        XCTAssertEqual(shift(150), -shift(450), accuracy: 0.001)
        for x in stride(from: CGFloat(-50), through: 650, by: 10) {
            XCTAssertLessThanOrEqual(
                abs(shift(x)),
                SidebarSessionDragController.dropZoneMagnetMaxShift
            )
        }
        // Continuity: no step in the curve bigger than the cursor step.
        var previous = shift(-50)
        for x in stride(from: CGFloat(-49), through: 650, by: 1) {
            let current = shift(x)
            XCTAssertLessThan(abs(current - previous), 2)
            previous = current
        }
        XCTAssertEqual(
            SidebarSessionDragController.dropZoneMagnetShift(
                cursorX: 10, zoneCenterX: 300, attractionRadius: 0
            ),
            0,
            "degenerate radius is inert"
        )
    }

    /// The square's mirror lean: toward the chip, settling centered when
    /// the chip reaches the zone middle, capped tighter than the card's
    /// pull so the two magnets never fight.
    func testDropZoneSquareLean() {
        func lean(_ x: CGFloat) -> CGFloat {
            SidebarSessionDragController.dropZoneSquareLean(
                cursorX: x, zoneCenterX: 300
            )
        }
        XCTAssertEqual(lean(300), 0, "centered chip, centered square")
        XCTAssertLessThan(lean(260), 0, "chip left of center pulls the square left")
        XCTAssertGreaterThan(lean(340), 0, "chip right of center pulls it right")
        XCTAssertEqual(lean(280), -3.6, accuracy: 0.001)
        XCTAssertEqual(
            lean(0),
            -SidebarSessionDragController.dropZoneSquareLeanMax,
            "far chip hits the cap"
        )
        XCTAssertEqual(
            lean(900),
            SidebarSessionDragController.dropZoneSquareLeanMax
        )
    }

    func testPinnedSourcesResolveToTheRightSidebarBoundary() {
        typealias Controller = SidebarSessionDragController

        XCTAssertEqual(
            Controller.pinnedBoundary(for: .session(
                projectID: "group", pinned: true, depth: 1
            )),
            .partition(parentID: "group")
        )
        XCTAssertNil(Controller.pinnedBoundary(for: .session(
            projectID: "group", pinned: false, depth: 1
        )))
        XCTAssertEqual(
            Controller.pinnedBoundary(for: .folderItem(
                parentID: "root", pinned: true, depth: 1
            )),
            .partition(parentID: "root"),
            "a pinned session and a pinned child group share the parent's one pinned partition"
        )
        XCTAssertNil(Controller.pinnedBoundary(for: .folderItem(
            parentID: "root", pinned: false, depth: 1
        )))
    }

    func testPinnedSessionReordersAtHomeButRejectsCrossGroupFiling() {
        typealias Controller = SidebarSessionDragController
        let boundary = Controller.PinnedDragBoundary.partition(parentID: "group")

        XCTAssertEqual(
            Controller.pinnedSidebarHitAction(
                boundary: boundary,
                sourceID: "pinned-a",
                targetID: "pinned-b",
                targetKind: .session(projectID: "group", pinned: true, depth: 1),
                firstReorderID: "pinned-a"
            ),
            .reorder(anchorID: "pinned-b"),
            "pinned Sessions remain sortable inside their pinned partition"
        )
        XCTAssertEqual(
            Controller.pinnedSidebarHitAction(
                boundary: boundary,
                sourceID: "pinned-a",
                targetID: "regular",
                targetKind: .session(projectID: "group", pinned: false, depth: 1),
                firstReorderID: "pinned-a"
            ),
            .deny,
            "the regular section is outside the pin boundary"
        )
        XCTAssertEqual(
            Controller.pinnedSidebarHitAction(
                boundary: boundary,
                sourceID: "pinned-a",
                targetID: "other-group",
                targetKind: .folderItem(parentID: "root", pinned: true, depth: 1),
                firstReorderID: "pinned-a"
            ),
            .deny,
            "a pinned Session cannot be filed into another group by drag"
        )
        XCTAssertEqual(
            Controller.pinnedSidebarHitAction(
                boundary: boundary,
                sourceID: "pinned-a",
                targetID: "sibling-group",
                targetKind: .folderItem(parentID: "group", pinned: true, depth: 1),
                firstReorderID: "pinned-a"
            ),
            .reorder(anchorID: "sibling-group"),
            "a pinned sibling GROUP is an ordinary anchor in the mixed pinned partition"
        )
        XCTAssertEqual(
            Controller.pinnedSidebarHitAction(
                boundary: boundary,
                sourceID: "pinned-a",
                targetID: "group",
                targetKind: .folderItem(parentID: "root", pinned: false, depth: 1),
                firstReorderID: "pinned-b"
            ),
            .reorder(anchorID: "pinned-b"),
            "the owning group header remains the top pinned-partition target"
        )
    }

    func testPinnedGroupSortsAcrossTheWholeMixedPinnedPartition() {
        typealias Controller = SidebarSessionDragController
        let boundary = Controller.PinnedDragBoundary.partition(parentID: "root")

        XCTAssertEqual(
            Controller.pinnedSidebarHitAction(
                boundary: boundary,
                sourceID: "group-a",
                targetID: "group-b",
                targetKind: .folderItem(parentID: "root", pinned: true, depth: 1),
                firstReorderID: "group-a"
            ),
            .reorder(anchorID: "group-b")
        )
        XCTAssertEqual(
            Controller.pinnedSidebarHitAction(
                boundary: boundary,
                sourceID: "group-a",
                targetID: "pinned-session",
                targetKind: .session(projectID: "root", pinned: true, depth: 0),
                firstReorderID: "group-a"
            ),
            .reorder(anchorID: "pinned-session"),
            "a pinned group interleaves with the parent's pinned sessions"
        )
        XCTAssertEqual(
            Controller.pinnedSidebarHitAction(
                boundary: boundary,
                sourceID: "group-a",
                targetID: "member-session",
                targetKind: .session(projectID: "group-a", pinned: false, depth: 1),
                firstReorderID: "group-a"
            ),
            .allow,
            "the dragged group's own members stay home, never a move out"
        )
        XCTAssertEqual(
            Controller.pinnedSidebarHitAction(
                boundary: boundary,
                sourceID: "group-a",
                targetID: "group-c",
                targetKind: .folderItem(parentID: "root", pinned: false, depth: 1),
                firstReorderID: "group-a"
            ),
            .deny
        )
        XCTAssertEqual(
            Controller.pinnedSidebarHitAction(
                boundary: boundary,
                sourceID: "group-a",
                targetID: "session",
                targetKind: .session(projectID: "root", pinned: false, depth: 0),
                firstReorderID: "group-a"
            ),
            .deny
        )
    }

    func testPinnedSessionStillAcceptsARealPaneDropTarget() {
        XCTAssertFalse(
            SidebarSessionDragController.pinnedContentDropIsDenied(
                hasPaneDropTarget: true,
                isPaneTitleDrag: false
            )
        )
        XCTAssertTrue(
            SidebarSessionDragController.pinnedContentDropIsDenied(
                hasPaneDropTarget: false,
                isPaneTitleDrag: false
            )
        )
    }

    /// The refused-drop shake must rest at identity on every integer beat
    /// (an idle row is untransformed, and back-to-back shakes chain without
    /// a jump), oscillate inside a step, and decay to nothing by its end.
    func testDeniedShakeCurve() {
        for beat in 0...3 {
            XCTAssertEqual(
                SidebarDeniedShakeEffect.translationX(progress: CGFloat(beat)),
                0,
                "integer beats are rest positions"
            )
        }
        var sawMovement = false
        for phase in stride(from: CGFloat(0.02), to: 0.6, by: 0.02)
        where abs(SidebarDeniedShakeEffect.translationX(progress: phase)) > 1 {
            sawMovement = true
        }
        XCTAssertTrue(sawMovement, "the step actually wiggles")
        XCTAssertLessThan(
            abs(SidebarDeniedShakeEffect.translationX(progress: 0.97)),
            0.5,
            "the wiggle decays before the beat completes"
        )
        for phase in stride(from: CGFloat(0), through: 3, by: 0.01) {
            XCTAssertLessThanOrEqual(
                abs(SidebarDeniedShakeEffect.translationX(progress: phase)),
                SidebarDeniedShakeEffect.amplitude
            )
        }
    }
}
