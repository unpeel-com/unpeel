import XCTest
@testable import UnpeelNative

final class SurfaceCacheEvictionTests: XCTestCase {
    func testReclaimedPaneRejectsItsStaleDeferredTeardown() {
        var evictions = DeferredSurfaceEvictions<String>()
        let staleToken = evictions.schedule("original-pane", for: "session")

        XCTAssertTrue(evictions.contains("session"))
        XCTAssertEqual(evictions.reclaim("session"), "original-pane")
        XCTAssertFalse(evictions.contains("session"))
        XCTAssertNil(evictions.take("session", token: staleToken))
    }

    func testRescheduledPaneCanOnlyBeClaimedByItsLatestToken() {
        var evictions = DeferredSurfaceEvictions<String>()
        let staleToken = evictions.schedule("first-pane", for: "session")
        XCTAssertEqual(evictions.reclaim("session"), "first-pane")

        let currentToken = evictions.schedule("same-pane", for: "session")

        XCTAssertNil(evictions.take("session", token: staleToken))
        XCTAssertEqual(
            evictions.take("session", token: currentToken),
            "same-pane"
        )
        XCTAssertFalse(evictions.contains("session"))
    }

    @MainActor
    func testLiveStyleSignatureChangesWithWorkspaceTint() {
        let previousHue = Theme.appTintHue
        let previousStrength = Theme.appTintStrength
        defer {
            Theme.appTintHue = previousHue
            Theme.appTintStrength = previousStrength
        }

        Theme.appTintHue = nil
        Theme.appTintStrength = 1
        let neutral = TerminalFrameStyle.resolved(
            command: "/bin/zsh",
            workingDirectory: nil
        )
        let neutralSignature = SurfaceCache.styleSignature(
            background: neutral.background,
            canvasSample: nil,
            paneStyle: neutral.paneStyle
        )

        Theme.appTintHue = AppTint.blue.hue
        let tinted = TerminalFrameStyle.resolved(
            command: "/bin/zsh",
            workingDirectory: nil
        )
        let tintedSignature = SurfaceCache.styleSignature(
            background: tinted.background,
            canvasSample: nil,
            paneStyle: tinted.paneStyle
        )

        XCTAssertNotEqual(neutralSignature, tintedSignature)
    }
}
