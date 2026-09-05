//
//  SidebarPagerMathTests.swift
//  UnpeelNativeTests
//
//  The workspace carousel's pure decision rules (SidebarPagerMath): rubber
//  banding, neighbor-slot wrapping, gesture-end commit rules, and the
//  click-slide direction/wrap plan. These are the state-machine inputs the
//  interactive pager and the programmatic single-slide both run on.
//

import XCTest
@testable import UnpeelNative

final class SidebarPagerMathTests: XCTestCase {
    // MARK: rubberBand

    func testRubberBandIsNearIdentityForSmallTravel() {
        // tanh(x) ≈ x for small x: 1:1 finger tracking near rest.
        let cap: CGFloat = 100
        let tracked = SidebarPagerMath.rubberBand(5, cap: cap)
        XCTAssertEqual(tracked, 5, accuracy: 0.1)
    }

    func testRubberBandSaturatesAtCap() {
        let cap: CGFloat = 100
        let far = SidebarPagerMath.rubberBand(10_000, cap: cap)
        XCTAssertLessThanOrEqual(far, cap)
        XCTAssertGreaterThan(far, cap * 0.99)
    }

    func testRubberBandIsOddSymmetric() {
        let cap: CGFloat = 80
        for travel: CGFloat in [1, 17, 60, 200, 1_000] {
            XCTAssertEqual(
                SidebarPagerMath.rubberBand(-travel, cap: cap),
                -SidebarPagerMath.rubberBand(travel, cap: cap),
                accuracy: 0.0001
            )
        }
    }

    func testRubberBandIsMonotonic() {
        let cap: CGFloat = 120
        var last: CGFloat = -1
        for travel in stride(from: CGFloat(0), through: 600, by: 10) {
            let value = SidebarPagerMath.rubberBand(travel, cap: cap)
            XCTAssertGreaterThanOrEqual(value, last)
            last = value
        }
    }

    func testRubberBandZeroCapAndZeroTravel() {
        XCTAssertEqual(SidebarPagerMath.rubberBand(50, cap: 0), 0)
        XCTAssertEqual(SidebarPagerMath.rubberBand(0, cap: 100), 0)
    }

    // MARK: neighborIndex

    func testNeighborIndexStepsForwardAndBackward() {
        let next = SidebarPagerMath.neighborIndex(
            scopedIndex: 1, direction: 1, count: 4
        )
        XCTAssertEqual(next.index, 2)
        XCTAssertFalse(next.wraps)

        let previous = SidebarPagerMath.neighborIndex(
            scopedIndex: 1, direction: -1, count: 4
        )
        XCTAssertEqual(previous.index, 0)
        XCTAssertFalse(previous.wraps)
    }

    func testNeighborIndexWrapsAtBothEnds() {
        let pastEnd = SidebarPagerMath.neighborIndex(
            scopedIndex: 3, direction: 1, count: 4
        )
        XCTAssertEqual(pastEnd.index, 0)
        XCTAssertTrue(pastEnd.wraps)

        let beforeStart = SidebarPagerMath.neighborIndex(
            scopedIndex: 0, direction: -1, count: 4
        )
        XCTAssertEqual(beforeStart.index, 3)
        XCTAssertTrue(beforeStart.wraps)
    }

    func testNeighborIndexTwoWorkspacesAlwaysTheOther() {
        // count = 2: both directions land on the other slot; only steps
        // past an end are wraps.
        let forward = SidebarPagerMath.neighborIndex(
            scopedIndex: 0, direction: 1, count: 2
        )
        XCTAssertEqual(forward.index, 1)
        XCTAssertFalse(forward.wraps)

        let backward = SidebarPagerMath.neighborIndex(
            scopedIndex: 0, direction: -1, count: 2
        )
        XCTAssertEqual(backward.index, 1)
        XCTAssertTrue(backward.wraps)
    }

    // MARK: shouldCommit

    private func commits(
        travel: CGFloat,
        velocity: CGFloat,
        width: CGFloat = 300
    ) -> Bool {
        SidebarPagerMath.shouldCommit(
            travel: travel,
            velocity: velocity,
            width: width,
            flickVelocity: 500,
            flickMinTravel: 30
        )
    }

    func testCommitByTravelThreshold() {
        // ≥ width/3 commits regardless of velocity.
        XCTAssertTrue(commits(travel: 100, velocity: 0))
        XCTAssertTrue(commits(travel: -100, velocity: 0))
        XCTAssertFalse(commits(travel: 99, velocity: 0))
    }

    func testFlickCommitNeedsVelocityAndMinTravel() {
        XCTAssertTrue(commits(travel: -40, velocity: -800))
        // Fast but not enough same-direction travel.
        XCTAssertFalse(commits(travel: -20, velocity: -800))
        // Enough travel but too slow.
        XCTAssertFalse(commits(travel: -40, velocity: -300))
    }

    func testFlickMustPointTheSameWayAsTravel() {
        // A bounce-back (velocity opposing the accumulated travel) must
        // never commit.
        XCTAssertFalse(commits(travel: -40, velocity: 800))
        XCTAssertFalse(commits(travel: 40, velocity: -800))
        XCTAssertTrue(commits(travel: 40, velocity: 800))
    }

    func testZeroTravelNeverCommits() {
        XCTAssertFalse(commits(travel: 0, velocity: 10_000))
    }

    // MARK: slidePlan

    func testSlidePlanTakesTheShorterDirection() {
        // 0 → 1 of 5: forward.
        let forward = SidebarPagerMath.slidePlan(
            currentIndex: 0, targetIndex: 1, count: 5
        )
        XCTAssertEqual(forward.direction, 1)
        XCTAssertFalse(forward.isWrap)

        // 0 → 4 of 5: one step backward beats four forward, wrapping.
        let backward = SidebarPagerMath.slidePlan(
            currentIndex: 0, targetIndex: 4, count: 5
        )
        XCTAssertEqual(backward.direction, -1)
        XCTAssertTrue(backward.isWrap)
    }

    func testSlidePlanEquidistantTieGoesForward() {
        // 0 → 2 of 4: two steps either way; forward wins, no wrap.
        let plan = SidebarPagerMath.slidePlan(
            currentIndex: 0, targetIndex: 2, count: 4
        )
        XCTAssertEqual(plan.direction, 1)
        XCTAssertFalse(plan.isWrap)
    }

    func testSlidePlanForwardWrapDetection() {
        // 3 → 0 of 4: forward one step around the end.
        let plan = SidebarPagerMath.slidePlan(
            currentIndex: 3, targetIndex: 0, count: 4
        )
        XCTAssertEqual(plan.direction, 1)
        XCTAssertTrue(plan.isWrap)
    }

    func testSlidePlanBackwardNoWrap() {
        // 3 → 2 of 4: plain backward step inside the row.
        let plan = SidebarPagerMath.slidePlan(
            currentIndex: 3, targetIndex: 2, count: 4
        )
        XCTAssertEqual(plan.direction, -1)
        XCTAssertFalse(plan.isWrap)
    }

    // MARK: pageOffsets / committedOffset (carousel page x-position math)

    func testPageOffsetsForwardParksNeighborBeyondTrailingEdge() {
        // Direction +1 (next workspace): the neighbor page parks exactly one
        // page width beyond the trailing edge and rides the container 1:1.
        let atRest = SidebarPagerMath.pageOffsets(
            containerOffset: 0, direction: 1, width: 300
        )
        XCTAssertEqual(atRest.live, 0)
        XCTAssertEqual(atRest.neighbor, 300)

        let dragged = SidebarPagerMath.pageOffsets(
            containerOffset: -80, direction: 1, width: 300
        )
        XCTAssertEqual(dragged.live, -80)
        XCTAssertEqual(dragged.neighbor, 220)
    }

    func testPageOffsetsBackwardParksNeighborBeyondLeadingEdge() {
        // Direction -1 (previous workspace): parked one width beyond the
        // leading edge, same shared translation.
        let atRest = SidebarPagerMath.pageOffsets(
            containerOffset: 0, direction: -1, width: 300
        )
        XCTAssertEqual(atRest.live, 0)
        XCTAssertEqual(atRest.neighbor, -300)

        let dragged = SidebarPagerMath.pageOffsets(
            containerOffset: 80, direction: -1, width: 300
        )
        XCTAssertEqual(dragged.live, 80)
        XCTAssertEqual(dragged.neighbor, -220)
    }

    func testCommittedOffsetRestsNeighborAtExactlyZeroBothDirections() {
        // The zero-seam invariant: a committed slide must land the entering
        // page at x = 0 exactly (the swap then replaces it with the live
        // page at the same x), for both directions.
        for direction in [1, -1] {
            let committed = SidebarPagerMath.committedOffset(
                direction: direction, width: 300
            )
            let pages = SidebarPagerMath.pageOffsets(
                containerOffset: committed, direction: direction, width: 300
            )
            XCTAssertEqual(
                pages.neighbor, 0,
                "entering page must rest at x = 0 (direction \(direction))"
            )
            // The outgoing live page sits exactly one page over.
            XCTAssertEqual(pages.live, direction == 1 ? -300 : 300)
        }
    }

    func testWrapSlideUsesTheSamePageMathAsAnyStep() {
        // A wrap only affects the dots blob — page geometry is identical to
        // any other step in the same direction. Forward wrap: 3 → 0 of 4.
        let forward = SidebarPagerMath.slidePlan(
            currentIndex: 3, targetIndex: 0, count: 4
        )
        XCTAssertEqual(forward.direction, 1)
        XCTAssertTrue(forward.isWrap)
        let forwardParked = SidebarPagerMath.pageOffsets(
            containerOffset: 0, direction: forward.direction, width: 260
        )
        XCTAssertEqual(forwardParked.neighbor, 260)
        let forwardLanded = SidebarPagerMath.pageOffsets(
            containerOffset: SidebarPagerMath.committedOffset(
                direction: forward.direction, width: 260
            ),
            direction: forward.direction, width: 260
        )
        XCTAssertEqual(forwardLanded.neighbor, 0)

        // Backward wrap: 0 → 3 of 4 takes one step around the start.
        let backward = SidebarPagerMath.slidePlan(
            currentIndex: 0, targetIndex: 3, count: 4
        )
        XCTAssertEqual(backward.direction, -1)
        XCTAssertTrue(backward.isWrap)
        let backwardParked = SidebarPagerMath.pageOffsets(
            containerOffset: 0, direction: backward.direction, width: 260
        )
        XCTAssertEqual(backwardParked.neighbor, -260)
        let backwardLanded = SidebarPagerMath.pageOffsets(
            containerOffset: SidebarPagerMath.committedOffset(
                direction: backward.direction, width: 260
            ),
            direction: backward.direction, width: 260
        )
        XCTAssertEqual(backwardLanded.neighbor, 0)
    }

    // MARK: isTranslated (the session-drag suppression latch)

    /// The detached session drag gates on `isTranslated`: it must hold for
    /// the WHOLE life of a gesture-plus-commit (translated container,
    /// materialized neighbor page, pending commit swap) and release only
    /// once the container is truly back at rest.
    @MainActor
    func testPagerIsTranslatedLatchAcrossGestureAndCommit() {
        let pager = SidebarWorkspacePager()
        let neighbor = WorkspaceListRowModel(
            id: "host-b",
            name: "Host B",
            detail: "",
            icon: "server.rack",
            badge: nil,
            home: nil,
            tint: .none,
            kind: .local(
                record: nil,
                isDefault: false,
                isCurrentInstance: false,
                isRunning: false
            )
        )

        XCTAssertFalse(pager.isTranslated, "at rest before any gesture")
        pager.begin(width: 300)
        XCTAssertFalse(pager.isTranslated, "begin re-bases to rest")

        pager.update(
            travel: -40,
            neighborRow: neighbor,
            direction: 1,
            commitDistance: 100,
            wraps: false
        )
        XCTAssertTrue(pager.isTranslated, "tracking fingers translates")

        var selected: String?
        pager.finish(commit: true) { selected = $0.id }
        XCTAssertTrue(
            pager.isTranslated,
            "commit slide + pending swap still owns the surface"
        )
        XCTAssertNil(selected, "scope switch waits for the swap")

        pager.settle()
        XCTAssertEqual(selected, "host-b")
        XCTAssertFalse(pager.isTranslated, "settled commit is at rest")
    }
}
