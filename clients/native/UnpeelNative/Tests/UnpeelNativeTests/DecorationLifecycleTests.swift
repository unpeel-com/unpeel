import AppKit
import XCTest
@testable import UnpeelNative

@MainActor
final class DecorationLifecycleTests: XCTestCase {
    private final class PresentationWindow: NSWindow {
        var presented = true
        override var occlusionState: NSWindow.OcclusionState {
            presented ? [.visible] : []
        }

        func setPresented(_ value: Bool) {
            presented = value
            NotificationCenter.default.post(name: NSWindow.didChangeOcclusionStateNotification, object: self)
        }
    }

    func testMotionRequiresVisibleAttachedViewWithoutReducedMotion() {
        for attached in [false, true] {
            for visible in [false, true] {
                for hidden in [false, true] {
                    for reduced in [false, true] {
                        XCTAssertEqual(
                            DecorationLifecycleView.allowsMotion(
                                attached: attached, visible: visible, hidden: hidden, reduceMotion: reduced
                            ),
                            attached && visible && !hidden && !reduced
                        )
                    }
                }
            }
        }
    }

    func testShimmerStopsForOcclusionHiddenAncestorAndDetach() throws {
        try requireMotion()
        let window = makeWindow()
        let parent = NSView(frame: NSRect(x: 0, y: 0, width: 200, height: 30))
        window.contentView = parent
        let shimmer = ShimmerGradientLayerView(color: .white)
        shimmer.frame = parent.bounds
        parent.addSubview(shimmer)
        shimmer.layout()
        let gradient = try XCTUnwrap(shimmer.layer?.sublayers?.first)
        XCTAssertNotNil(gradient.animation(forKey: "shimmer"))

        window.setPresented(false)
        XCTAssertNil(gradient.animation(forKey: "shimmer"))
        window.setPresented(true)
        XCTAssertNotNil(gradient.animation(forKey: "shimmer"))

        parent.isHidden = true
        XCTAssertNil(gradient.animation(forKey: "shimmer"))
        parent.isHidden = false
        XCTAssertNotNil(gradient.animation(forKey: "shimmer"))

        shimmer.removeFromSuperview()
        XCTAssertNil(gradient.animation(forKey: "shimmer"))
    }

    func testUnchangedShimmerLayoutKeepsAnimationButResizeUpdatesTravel() throws {
        try requireMotion()
        let window = makeWindow()
        let shimmer = ShimmerGradientLayerView(color: .white)
        window.contentView?.addSubview(shimmer)
        shimmer.frame = NSRect(x: 0, y: 0, width: 100, height: 16)
        shimmer.layout()
        let gradient = try XCTUnwrap(shimmer.layer?.sublayers?.first)
        let original = try XCTUnwrap(gradient.animation(forKey: "shimmer")?.copy() as? CAAnimation)
        original.beginTime = 123
        gradient.add(original, forKey: "shimmer")
        shimmer.layout()
        XCTAssertEqual(gradient.animation(forKey: "shimmer")?.beginTime, 123)

        shimmer.frame.size.width = 150
        shimmer.layout()
        let resized = try XCTUnwrap(gradient.animation(forKey: "shimmer") as? CABasicAnimation)
        XCTAssertEqual(resized.toValue as? CGFloat, 300)
    }

    func testSpinnerRetainsStaticFrameWhileOccludedAndStopsOnDetach() throws {
        try requireMotion()
        let window = makeWindow()
        let spinner = SpinnerLayerView(color: .white)
        window.contentView?.addSubview(spinner)
        let layer = try XCTUnwrap(spinner.layer)
        XCTAssertNotNil(layer.animation(forKey: "spinner"))
        window.setPresented(false)
        XCTAssertNil(layer.animation(forKey: "spinner"))
        XCTAssertNotNil(layer.contents)
        window.setPresented(true)
        XCTAssertNotNil(layer.animation(forKey: "spinner"))
        spinner.isHidden = true
        XCTAssertNil(layer.animation(forKey: "spinner"))
        spinner.isHidden = false
        XCTAssertNotNil(layer.animation(forKey: "spinner"))
        spinner.removeFromSuperview()
        XCTAssertNil(layer.animation(forKey: "spinner"))
    }

    private func requireMotion() throws {
        _ = NSApplication.shared
        try XCTSkipIf(NSWorkspace.shared.accessibilityDisplayShouldReduceMotion,
                      "Layer playback checks require Reduce Motion off; policy coverage tests both preferences.")
    }

    private func makeWindow() -> PresentationWindow {
        let window = PresentationWindow(
            contentRect: NSRect(x: 0, y: 0, width: 200, height: 40),
            styleMask: [.borderless], backing: .buffered, defer: false
        )
        window.isReleasedWhenClosed = false
        return window
    }
}
