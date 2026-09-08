//
//  TerminalSurfaceCoordinator.swift
//  libghostty-spm
//
//  Created by Lakr233 on 2026/3/16.
//

import Foundation
import GhosttyKit

/// Shared terminal state and logic used by both UIKit and AppKit views.
///
/// Platform views own a `TerminalSurfaceCoordinator` instance and set platform-specific
/// hooks via closures. The core handles surface lifecycle, metrics
/// synchronization, and frame rendering via scheduled wakeups.
@MainActor
final class TerminalSurfaceCoordinator {
    weak var delegate: (any TerminalSurfaceViewDelegate)? {
        didSet { bridge.delegate = delegate }
    }

    var controller: TerminalController? {
        didSet {
            guard controller !== oldValue else { return }
            rebuildIfReady(removingBridgeFrom: oldValue)
        }
    }

    var configuration: TerminalSurfaceOptions = .init() {
        didSet {
            guard !configuration.isEquivalent(to: oldValue) else { return }
            rebuildIfReady()
        }
    }

    var surface: TerminalSurface?
    let bridge = TerminalCallbackBridge()

    // MARK: - Platform Hooks

    var isAttached: () -> Bool = { false }
    var isPresented: () -> Bool = { true }
    var scaleFactor: () -> Double = { 2.0 }
    var viewSize: () -> (width: Double, height: Double) = { (0, 0) }
    /// CGDirectDisplayID of the display hosting the view, for binding the
    /// renderer's CVDisplayLink to the right refresh rate (nil = unknown).
    var currentDisplayID: () -> UInt32? = { nil }
    var platformSetup: ((inout ghostty_surface_config_s) -> Void)?
    var onMetricsUpdate: (() -> Void)?
    var onCellSizeDidChange: (() -> Void)?

    /// Called after every requested render.
    ///
    /// When `synchronizeMetrics` sends a new pixel size to ghostty via
    /// `setSize`, the underlying IOSurface is not rebuilt synchronously.
    /// Until the next full render pass ghostty still uses the **old**
    /// IOSurface, so it derives an incorrect `contentsScale` for the
    /// IOSurfaceLayer (e.g. old-pixel-height / new-point-height → 4.62
    /// instead of the expected 3.0). This causes a visible "jump" on
    /// every layout change (keyboard show/hide, rotation, color-scheme
    /// toggle, etc.).
    ///
    /// Platform views use this hook to silently enforce the correct
    /// `contentsScale` and `frame` on sublayers after each render,
    /// correcting any drift introduced by ghostty within a single frame.
    var onPostRender: (() -> Void)?

    private var lastMetrics: TerminalViewportMetrics?
    private var lastSentPixelSize: (width: UInt32, height: UInt32)?
    private var lastSentScale: Double?
    private var pendingSynchronousDraw = false
    private var isDisplayVisible = true
    private var isApplicationActive = true
    private var isSurfaceFocused = false
    private lazy var renderScheduler = TerminalRenderScheduler { [weak self] in
        self?.renderImmediately()
    }

    init() {
        bridge.onCellSizeChange = { [weak self] width, height in
            self?.handleCellSizeChange(width: width, height: height)
        }
        bridge.onRenderRequest = { [weak self] in
            self?.requestImmediateTick()
        }
    }

    func requestImmediateTick() {
        guard canRenderFrame else { return }
        renderScheduler.request()
    }

    // MARK: - Event-driven refresh

    /// Scroll input needs an explicit refresh even when the core coalesces
    /// its render action. Later TUI output is rendered by the core; host-fed
    /// output also requests a refresh after each completed write.
    func noteRenderActivity() {
        requestImmediateTick()
    }

    /// Presents the current frame without blocking the main thread.
    ///
    /// `surface.draw()` maps to core `drawFrame(sync: true)`, which both
    /// forces a full redraw (even when cells are unchanged) and blocks on
    /// `waitUntilCompleted` — on long sessions that GPU wait ate ~a third
    /// of main-thread wall time during scroll, stalling input. Steady-state
    /// frames instead go through `refresh()`, which wakes the core
    /// renderer thread for a non-blocking `drawFrame(sync: false)`.
    ///
    /// The one frame after a pixel-size change still draws synchronously:
    /// main-thread draw is ghostty's live-resize contract (contents must
    /// update before the resized frame is presented), and `onPostRender`'s
    /// sublayer contentsScale/frame corrections rely on that draw having
    /// happened when the size just changed.
    private func presentFrame() {
        surface?.refresh()
        if pendingSynchronousDraw {
            pendingSynchronousDraw = false
            surface?.draw()
        }
        onPostRender?()
    }

    func refreshPresentationVisibility() {
        surface?.setOcclusion(effectiveSurfaceVisible)
        renderScheduler.setEnabled(canRenderFrame)
        requestImmediateTick()
    }

    func stopRendering() {
        surface?.setOcclusion(false)
        renderScheduler.setEnabled(false)
    }

    // MARK: - Surface Lifecycle

    func rebuildIfReady(removingBridgeFrom previousController: TerminalController? = nil) {
        // Controller removal is terminal destruction, so detach the Swift
        // state now but never join Ghostty's EXEC backend threads on the main
        // actor. A live controller/config rebuild remains synchronous because
        // the replacement surface immediately reuses this coordinator.
        tearDownSurface(
            removingBridgeFrom: previousController ?? controller,
            asynchronously: controller == nil
        )
        guard let controller else {
            TerminalDebugLog.log(.lifecycle, "surface rebuild skipped: missing controller")
            return
        }
        guard isAttached() else {
            TerminalDebugLog.log(.lifecycle, "surface rebuild skipped: view detached")
            return
        }
        guard hasValidViewSize else {
            let size = viewSize()
            TerminalDebugLog.log(
                .lifecycle,
                "surface rebuild skipped: invalid view size=\(String(format: "%.2f", size.width))x\(String(format: "%.2f", size.height))"
            )
            return
        }

        let scale = scaleFactor()
        TerminalDebugLog.log(
            .lifecycle,
            "surface rebuild scale=\(String(format: "%.2f", scale)) \(configuration.debugSummary)"
        )
        let newSurface = controller.createSurface(
            bridge: bridge,
            configuration: configuration,
            platformSetup: { [self] config in
                platformSetup?(&config)
                config.scale_factor = scale
            }
        )
        guard let newSurface else {
            TerminalDebugLog.log(.lifecycle, "surface rebuild failed")
            return
        }

        bridge.rawSurface = newSurface.rawValue
        surface = newSurface
        newSurface.setOcclusion(effectiveSurfaceVisible)
        if let displayID = currentDisplayID() {
            newSurface.setDisplayID(displayID)
        }
        controller.onWakeup = { [weak self] in
            self?.requestImmediateTick()
        }
        // Writes finish parsing before this notification. Coalesce transport
        // threads onto main without retaining or touching a raw surface.
        renderScheduler.setEnabled(canRenderFrame)
        let scheduler = renderScheduler
        configuration.inMemorySession?.onHostBytes = {
            scheduler.request()
        }
        TerminalDebugLog.log(.lifecycle, "surface rebuild succeeded")
        (delegate as? any TerminalSurfaceLifecycleDelegate)?
            .terminalDidAttachSurface(newSurface)
        synchronizeMetrics()
        requestImmediateTick()
    }

    // MARK: - Metrics

    func synchronizeMetrics() {
        guard let surface else {
            TerminalDebugLog.log(.metrics, "synchronizeMetrics skipped: missing surface")
            return
        }

        let scale = scaleFactor()
        let size = viewSize()
        guard size.width > 0, size.height > 0 else {
            TerminalDebugLog.log(
                .metrics,
                "synchronizeMetrics skipped: invalid view size=\(String(format: "%.2f", size.width))x\(String(format: "%.2f", size.height))"
            )
            return
        }

        let pixelWidth = UInt32((size.width * scale).rounded(.down))
        let pixelHeight = UInt32((size.height * scale).rounded(.down))
        guard pixelWidth > 0, pixelHeight > 0 else {
            TerminalDebugLog.log(
                .metrics,
                "synchronizeMetrics skipped: invalid pixel size=\(pixelWidth)x\(pixelHeight)"
            )
            return
        }

        TerminalDebugLog.log(
            .metrics,
            "sync view=\(String(format: "%.2f", size.width))x\(String(format: "%.2f", size.height)) scale=\(String(format: "%.2f", scale)) pixels=\(pixelWidth)x\(pixelHeight)"
        )

        if lastSentPixelSize?.width != pixelWidth || lastSentPixelSize?.height != pixelHeight {
            lastSentPixelSize = (pixelWidth, pixelHeight)
            pendingSynchronousDraw = true
        }
        lastSentScale = scale
        surface.setContentScale(x: scale, y: scale)
        surface.setSize(width: pixelWidth, height: pixelHeight)

        guard let surfaceSize = surface.size(),
              surfaceSize.columns > 0, surfaceSize.rows > 0
        else {
            TerminalDebugLog.log(.metrics, "sync missing grid metrics after resize")
            onMetricsUpdate?()
            return
        }

        let metrics = TerminalViewportMetrics(surfaceSize: surfaceSize, scale: scale)
        guard metrics != lastMetrics else {
            TerminalDebugLog.log(
                .metrics,
                "sync unchanged \(metrics.debugSummary)"
            )
            onMetricsUpdate?()
            return
        }

        lastMetrics = metrics
        TerminalDebugLog.log(.metrics, "sync updated \(metrics.debugSummary)")
        // Real size delivered: core's resize reports are meaningful now.
        configuration.inMemorySession?.armResizeDispatch(
            syncedWidthPixels: pixelWidth,
            syncedHeightPixels: pixelHeight
        )
        configuration.inMemorySession?.updateViewport(surfaceSize)
        if let delegate = delegate as? any TerminalSurfaceGridResizeDelegate {
            delegate.terminalDidResize(surfaceSize)
        } else if let delegate = delegate as? any TerminalSurfaceResizeDelegate {
            delegate.terminalDidResize(
                columns: Int(surfaceSize.columns),
                rows: Int(surfaceSize.rows)
            )
        }
        onMetricsUpdate?()
    }

    func fitToSize() {
        if surface == nil {
            rebuildIfReady()
        } else {
            synchronizeMetrics()
        }
        if surface != nil {
            requestImmediateTick()
        }
    }

    /// Re-fits like a real window resize. `fitToSize` on a re-attached view
    /// usually sends ghostty the same pixel size it already has, which is a
    /// no-op — any sublayer frame / contentsScale / grid drift accumulated
    /// while the view was detached or paused survives it. Nudging one pixel
    /// narrower first makes the follow-up `setSize` a genuine size change,
    /// so ghostty runs its full resize path. Both writes land before the
    /// next draw; the narrower size is never presented.
    func forceRefit() {
        guard let surface else {
            rebuildIfReady()
            if self.surface != nil {
                requestImmediateTick()
            }
            return
        }
        let scale = scaleFactor()
        let size = viewSize()
        let pixelWidth = UInt32((size.width * scale).rounded(.down))
        let pixelHeight = UInt32((size.height * scale).rounded(.down))
        guard pixelWidth > 1, pixelHeight > 0 else { return }
        TerminalDebugLog.log(.metrics, "force refit pixels=\(pixelWidth)x\(pixelHeight)")
        surface.setSize(width: pixelWidth - 1, height: pixelHeight)
        lastMetrics = nil
        lastSentPixelSize = nil
        lastSentScale = nil
        synchronizeMetrics()
        requestImmediateTick()
    }

    /// `forceRefit` only when the view's pixel size or scale actually
    /// differs from what the surface last received — i.e. layout or display
    /// change happened while the view was detached/paused and its resize
    /// events were swallowed. A clean re-adopt (the common session switch)
    /// skips the nudge, which otherwise costs two full ghostty resize
    /// passes plus PTY winsize churn that makes the running TUI repaint.
    func refitIfDrifted() {
        guard let surface else {
            forceRefit()
            return
        }
        let scale = scaleFactor()
        let size = viewSize()
        let pixelWidth = UInt32((size.width * scale).rounded(.down))
        let pixelHeight = UInt32((size.height * scale).rounded(.down))
        if let sent = lastSentPixelSize,
           sent.width == pixelWidth, sent.height == pixelHeight,
           lastSentScale == scale,
           surface.size() != nil {
            requestImmediateTick()
            return
        }
        forceRefit()
    }

    /// Re-bind the renderer's display link to the view's current display.
    /// Platform views call this on window/screen transitions.
    func refreshDisplayBinding() {
        guard let surface, let displayID = currentDisplayID() else { return }
        surface.setDisplayID(displayID)
    }

    func setDisplayVisible(_ visible: Bool) {
        isDisplayVisible = visible
        refreshPresentationVisibility()
    }

    func setApplicationActive(_ active: Bool) {
        guard isApplicationActive != active else {
            if active {
                renderImmediately()
            } else {
                stopRendering()
            }
            return
        }

        isApplicationActive = active
        surface?.setOcclusion(effectiveSurfaceVisible)
        renderScheduler.setEnabled(canRenderFrame)

        if active {
            synchronizeMetrics()
            renderImmediately()
        } else {
            stopRendering()
        }
    }

    // MARK: - Focus

    func setFocus(_ focused: Bool) {
        isSurfaceFocused = focused
        requestImmediateTick()
        TerminalDebugLog.log(.lifecycle, "focus=\(focused)")
        surface?.setFocus(focused)
        (delegate as? any TerminalSurfaceFocusDelegate)?
            .terminalDidChangeFocus(focused)
    }

    // MARK: - Cleanup

    func freeSurface() {
        TerminalDebugLog.log(.lifecycle, "free surface")
        tearDownSurface(removingBridgeFrom: controller, asynchronously: true)
    }

    deinit {
        // `@MainActor` classes have a nonisolated deinit by default, but
        // `tearDownSurface` calls methods on other main-actor types (surface,
        // bridge, controller). We rely on deinit running synchronously with
        // exclusive access; assume main-actor isolation so teardown can run
        // inline without crossing isolation.
        MainActor.assumeIsolated {
            tearDownSurface(removingBridgeFrom: controller, asynchronously: true)
        }
    }

    private func tearDownSurface(
        removingBridgeFrom controller: TerminalController?,
        asynchronously: Bool = false
    ) {
        TerminalDebugLog.log(.lifecycle, "tear down surface")
        renderScheduler.setEnabled(false)
        if let session = configuration.inMemorySession {
            session.onHostBytes = nil
            session.clearSurface(ifMatches: surface?.rawValue)
        }
        controller?.onWakeup = nil
        bridge.rawSurface = nil
        let hadSurface = surface != nil
        surface?.setFocus(false)
        if asynchronously {
            surface?.freeAsync()
        } else {
            surface?.free()
        }
        surface = nil
        lastMetrics = nil
        lastSentPixelSize = nil
        lastSentScale = nil
        pendingSynchronousDraw = false
        controller?.remove(bridge)
        if hadSurface {
            (delegate as? any TerminalSurfaceLifecycleDelegate)?
                .terminalDidDetachSurface()
        }
    }

    private func handleCellSizeChange(width: UInt32, height: UInt32) {
        TerminalDebugLog.log(
            .metrics,
            "cell size changed width=\(width) height=\(height)"
        )
        synchronizeMetrics()
        requestImmediateTick()
        onCellSizeDidChange?()
    }

    private var effectiveSurfaceVisible: Bool {
        isDisplayVisible && isApplicationActive && isAttached() && isPresented()
    }

    private var canRenderFrame: Bool {
        effectiveSurfaceVisible
    }

    private var hasValidViewSize: Bool {
        let size = viewSize()
        return size.width > 0 && size.height > 0
    }

    func renderImmediately() {
        renderScheduler.cancel()
        guard canRenderFrame else { return }
        presentFrame()
    }

    /// `renderImmediately`, but the frame draws synchronously even when the
    /// pixel size is unchanged. An occluded surface parses output without
    /// drawing, so its layer still holds the drawable from when it was last
    /// shown; the plain path only wakes the renderer thread (`refresh`),
    /// which presents that stale frame first and the current one a beat
    /// later. Session adoption uses this so the swap's own transaction
    /// already shows current content.
    func renderImmediately(synchronousDraw: Bool) {
        if synchronousDraw, surface != nil {
            pendingSynchronousDraw = true
        }
        renderImmediately()
    }
}
