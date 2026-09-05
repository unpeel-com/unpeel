import Foundation
import XCTest
@testable import UnpeelNative

final class RemoteGhosttyPaneRetentionTests: XCTestCase {
    @MainActor
    func testStaleContainerCannotDetachPaneFromItsReplacementMount() {
        let pane = RemoteGhosttyTerminalPane(
            onInput: { _ in },
            onResize: { _ in }
        )
        let oldContainer = RemoteTerminalPaneHostView.SwapContainer(
            frame: CGRect(x: 0, y: 0, width: 100, height: 80)
        )
        let replacement = RemoteTerminalPaneHostView.SwapContainer(
            frame: CGRect(x: 0, y: 0, width: 300, height: 180)
        )

        XCTAssertTrue(oldContainer.attach(pane))
        XCTAssertTrue(replacement.attach(pane))
        let replacementFrame = pane.frame
        oldContainer.layout()
        oldContainer.detachPane()

        XCTAssertEqual(pane.frame, replacementFrame)
        XCTAssertFalse(oldContainer.hosts(pane))
        XCTAssertTrue(replacement.hosts(pane))
        XCTAssertTrue(pane.superview === replacement)
        XCTAssertTrue(replacement.attachedPane === pane)
    }

    @MainActor
    func testCachePruneProtectsEveryVisiblePaneAboveItsNominalLimit() {
        let primaryKey = RemoteTerminalPaneKey(hostID: "host", sessionID: "primary")
        let companionKey = RemoteTerminalPaneKey(hostID: "host", sessionID: "companion")
        let backgroundKey = RemoteTerminalPaneKey(hostID: "host", sessionID: "background")
        let cache = RemoteGhosttyPaneCache(retainedPaneLimit: 1)

        let primary = cache.pane(
            for: primaryKey,
            onInput: { _ in },
            onResize: { _ in }
        )
        let companion = cache.pane(
            for: companionKey,
            onInput: { _ in },
            onResize: { _ in }
        )
        _ = cache.pane(
            for: backgroundKey,
            onInput: { _ in },
            onResize: { _ in }
        )

        cache.prune(
            keeping: Set([primaryKey, companionKey, backgroundKey]),
            selectedKey: primaryKey,
            protectedKeys: Set([companionKey])
        )

        XCTAssertTrue(cache.existingPane(for: primaryKey) === primary)
        XCTAssertTrue(cache.existingPane(for: companionKey) === companion)
        XCTAssertNil(cache.existingPane(for: backgroundKey))
    }

    @MainActor
    func testDetachedPaneRefusesOutputAndResetBeforeAnythingCanBuffer() {
        let pane = RemoteGhosttyTerminalPane(
            onInput: { _ in },
            onResize: { _ in }
        )

        XCTAssertFalse(pane.isReadyForHostBytes)
        XCTAssertFalse(
            pane.receiveHostBytes(Data("must remain uncommitted".utf8))
        )
        XCTAssertFalse(
            pane.receiveHostBytes(
                Data("fresh tail must remain uncommitted".utf8),
                resetBeforeFeed: true
            )
        )
        XCTAssertFalse(pane.resetRetainedVTState())
    }

    func testCallbackEpochRejectsWorkQueuedBeforeRebind() {
        var current = RemoteTerminalCallbackEpoch()
        let queuedBeforeRebind = current

        XCTAssertTrue(current.accepts(queuedBeforeRebind))
        current.advance()
        XCTAssertFalse(current.accepts(queuedBeforeRebind))
    }

    func testLocalResetFeedUsesRISClearAndSynchronizedReplacement() {
        let payload = Data("replacement".utf8)
        let reset = Data([0x18, 0x1B, 0x63])
        let begin = Data("\u{1B}[?2026h".utf8)
        let clear = Data("\u{1B}[3J\u{1B}[2J\u{1B}[H".utf8)
        let end = Data("\u{1B}[?2026l".utf8)

        XCTAssertEqual(
            RemoteTerminalLocalFeed.resetRetainedState.bytes,
            reset + begin + clear + end
        )
        XCTAssertEqual(
            RemoteTerminalLocalFeed.resettingBeforeFeeding(payload).bytes,
            reset + begin + clear + payload + end
        )
    }

    func testSessionIdentityIsScopedByHost() {
        let studio = RemoteTerminalPaneKey(hostID: "studio", sessionID: "same-session")
        let server = RemoteTerminalPaneKey(hostID: "server", sessionID: "same-session")

        XCTAssertNotEqual(studio, server)
        XCTAssertEqual(Set([studio, server]).count, 2)
    }

    func testRetentionKeepsMostRecentPanesWithinLimit() {
        let first = RemoteTerminalPaneKey(hostID: "host", sessionID: "first")
        let second = RemoteTerminalPaneKey(hostID: "host", sessionID: "second")
        let third = RemoteTerminalPaneKey(hostID: "host", sessionID: "third")
        var retention = RemoteTerminalPaneRetention(limit: 2)

        retention.noteUsed(first)
        retention.noteUsed(second)
        retention.noteUsed(third)

        XCTAssertEqual(
            retention.retained(from: Set([first, second, third])),
            Set([second, third])
        )
    }

    func testSelectedPaneIsProtectedEvenWhenNotRecent() {
        let selected = RemoteTerminalPaneKey(hostID: "host", sessionID: "selected")
        let recent = RemoteTerminalPaneKey(hostID: "host", sessionID: "recent")
        let newest = RemoteTerminalPaneKey(hostID: "host", sessionID: "newest")
        var retention = RemoteTerminalPaneRetention(limit: 2)

        retention.noteUsed(selected)
        retention.noteUsed(recent)
        retention.noteUsed(newest)

        XCTAssertEqual(
            retention.retained(
                from: Set([selected, recent, newest]),
                protecting: Set([selected])
            ),
            Set([selected, newest])
        )
    }

    func testUnavailablePanesAreRemovedFromHistory() {
        let stale = RemoteTerminalPaneKey(hostID: "old-host", sessionID: "stale")
        let live = RemoteTerminalPaneKey(hostID: "new-host", sessionID: "live")
        var retention = RemoteTerminalPaneRetention(limit: 2)

        retention.noteUsed(stale)
        retention.noteUsed(live)

        XCTAssertEqual(retention.retained(from: Set([live])), Set([live]))
        XCTAssertEqual(retention.mostRecent, [live])
    }
}
