//
//  AppTerminalView+Lifecycle.swift
//  libghostty-spm
//
//  Created by Lakr233 on 2026/3/17.
//

#if canImport(AppKit) && !canImport(UIKit)
    import AppKit

    extension AppTerminalView {
        func setupTrackingArea() {
            let options: NSTrackingArea.Options = [
                .mouseEnteredAndExited,
                .mouseMoved,
                .inVisibleRect,
                .activeAlways,
            ]
            let area = NSTrackingArea(
                rect: bounds,
                options: options,
                owner: self,
                userInfo: nil
            )
            addTrackingArea(area)
        }

        override open func updateTrackingAreas() {
            super.updateTrackingAreas()
            trackingAreas.forEach { removeTrackingArea($0) }
            setupTrackingArea()
        }

        override open var acceptsFirstResponder: Bool {
            true
        }

        override open func becomeFirstResponder() -> Bool {
            let result = super.becomeFirstResponder()
            core.setFocus(true)
            onFocusChange?(true)
            return result
        }

        override open func resignFirstResponder() -> Bool {
            let result = super.resignFirstResponder()
            core.setFocus(false)
            onFocusChange?(false)
            return result
        }

        override open func viewDidMoveToWindow() {
            super.viewDidMoveToWindow()
            removeWindowObservers()
            if window != nil {
                // SwiftUI/AppKit can temporarily detach and reattach the terminal view while
                // diffing the view hierarchy. Rebuilding on every reattach discards Ghostty's
                // scrollback/state, so only create a new surface when one does not already exist.
                if surface == nil {
                    core.rebuildIfReady()
                } else {
                    core.synchronizeMetrics()
                }
                // Bind the renderer's display link to the display this
                // window is actually on: the link is created against a
                // default display and never follows the window on its own,
                // which caps vsync draws at the wrong refresh rate on
                // ProMotion / multi-display setups.
                core.refreshDisplayBinding()
                TerminalDebugLog.log(
                    .lifecycle,
                    "screen maxFPS=\(window?.screen?.maximumFramesPerSecond ?? -1)"
                )
                updateMetalLayerMetrics()
                updateColorScheme()
                core.startDisplayLink()
                core.requestImmediateTick()

                NotificationCenter.default.addObserver(
                    self,
                    selector: #selector(windowDidBecomeKey),
                    name: NSWindow.didBecomeKeyNotification,
                    object: window
                )
                NotificationCenter.default.addObserver(
                    self,
                    selector: #selector(windowDidResignKey),
                    name: NSWindow.didResignKeyNotification,
                    object: window
                )
                // Cross-display rescue: AppKit posts didChangeScreen when the
                // window's screen reference changes, even when the new screen
                // has the same backingScaleFactor (in which case
                // viewDidChangeBackingProperties does not fire). Listening
                // here lets us re-run metric sync on every screen transition
                // — required for the case where two displays share scale but
                // differ in geometry / color profile, and harmless when
                // viewDidChangeBackingProperties also fires for the
                // different-scale case.
                NotificationCenter.default.addObserver(
                    self,
                    selector: #selector(windowDidChangeScreen),
                    name: NSWindow.didChangeScreenNotification,
                    object: window
                )
            } else {
                core.stopDisplayLink()
                core.setFocus(false)
            }
        }

        @objc func windowDidBecomeKey(_: Notification) {
            let focused = window?.isKeyWindow == true
                && window?.firstResponder === self
            core.setFocus(focused)
            onFocusChange?(focused)
        }

        @objc func windowDidResignKey(_: Notification) {
            core.setFocus(false)
            onFocusChange?(false)
        }

        @objc func windowDidChangeScreen(_: Notification) {
            // Defer one runloop tick so AppKit's layout pass and the
            // window's new backingScaleFactor have both settled before we
            // re-derive metrics. Calling synchronously can race with the
            // layout pass and re-introduce the drift we're trying to fix.
            DispatchQueue.main.async { [weak self] in
                guard let self else { return }
                core.refreshDisplayBinding()
                TerminalDebugLog.log(
                    .lifecycle,
                    "screen maxFPS=\(self.window?.screen?.maximumFramesPerSecond ?? -1)"
                )
                updateMetalLayerMetrics()
                core.synchronizeMetrics()
                core.requestImmediateTick()
            }
        }

        private func removeWindowObservers() {
            // Remove any existing key-window observers before registering for the
            // current window. AppKit can move the view directly between windows
            // without an intermediate nil attachment.
            NotificationCenter.default.removeObserver(
                self,
                name: NSWindow.didBecomeKeyNotification,
                object: nil
            )
            NotificationCenter.default.removeObserver(
                self,
                name: NSWindow.didResignKeyNotification,
                object: nil
            )
            NotificationCenter.default.removeObserver(
                self,
                name: NSWindow.didChangeScreenNotification,
                object: nil
            )
        }

        override open func setFrameSize(_ newSize: NSSize) {
            super.setFrameSize(newSize)
            core.fitToSize()
            // Draw in the SAME runloop turn as the frame change. The old
            // requestImmediateTick() deferred the draw one main-queue hop,
            // so every resize frame presented the previous drawable against
            // the new bounds first — the resize "shake". Synchronous render
            // closes that gap.
            core.renderImmediately()
        }

        override open func layout() {
            super.layout()
            // Never fit while detached: with no window the scale falls back
            // to NSScreen.main, which can be a different-DPI display. A fit
            // submitted at that wrong scale is processed serially by core
            // and its resize lands AFTER the re-attach fits — shrinking a
            // host-managed remote PTY to the wrong grid (e.g. a 2x pane
            // half-sized by a 1x main-display fallback). Re-attaching
            // always runs a fresh layout + fit with the real window scale.
            guard window != nil else { return }
            core.fitToSize()
            core.renderImmediately()
        }

        override open func viewDidChangeBackingProperties() {
            super.viewDidChangeBackingProperties()
            // Same detached guard as layout(): with no window this fires
            // with fallback scale and would submit a wrong-DPI fit.
            guard window != nil else { return }
            updateMetalLayerMetrics()
            core.fitToSize()
            core.renderImmediately()
        }

        public func fitToSize() {
            core.fitToSize()
        }

        /// Runs the same path as a live window resize even when the view's
        /// size is unchanged (see `TerminalSurfaceCoordinator.forceRefit`).
        /// For hosts that swap retained terminal views between superviews:
        /// call after re-attaching once layout has settled.
        public func forceRefit() {
            updateMetalLayerMetrics()
            core.forceRefit()
        }

        /// Like `forceRefit`, but skips the forced resize pass when the
        /// view's pixel size and scale still match what the surface last
        /// received — the common no-drift session switch stays free of
        /// resize churn (and of the PTY winsize double-SIGWINCH it causes).
        public func refitIfDrifted() {
            updateMetalLayerMetrics()
            core.refitIfDrifted()
        }

        func updateMetalLayerMetrics() {
            guard bounds.width > 0, bounds.height > 0 else { return }
            let scale = core.scaleFactor()
            // Write to the actually-attached backing layer (not just the
            // cached `metalLayer` ivar). The render pipeline can swap
            // `self.layer` to an IOSurfaceLayer for IOSurface-backed
            // compositing; once that happens the cached CAMetalLayer
            // reference is detached from the view tree and writes to its
            // contentsScale are no-ops as far as what's visible. The
            // observable symptom is text rendered at half size after the
            // window crosses to a display with a different
            // backingScaleFactor.
            // Guard every write: setting drawableSize invalidates the
            // drawable pool even for an identical value, and this runs per
            // resize frame (plus `layer` and `metalLayer` are usually the
            // SAME CAMetalLayer, which doubled the invalidation).
            let drawableSize = CGSize(
                width: bounds.width * scale,
                height: bounds.height * scale
            )
            if let layer, layer.contentsScale != scale {
                layer.contentsScale = scale
            }
            if let metal = layer as? CAMetalLayer, metal.drawableSize != drawableSize {
                metal.drawableSize = drawableSize
            }
            // Mirror to the cached ivar in case anything else still
            // reads through it during a transitional layout pass.
            if let metalLayer {
                if metalLayer.contentsScale != scale {
                    metalLayer.contentsScale = scale
                }
                if metalLayer.drawableSize != drawableSize {
                    metalLayer.drawableSize = drawableSize
                }
            }
        }

        func enforceMetalLayerScale() {
            let scale = core.scaleFactor()
            if let layer, layer.contentsScale != scale {
                layer.contentsScale = scale
            }
            if let metalLayer, metalLayer.contentsScale != scale {
                metalLayer.contentsScale = scale
            }
        }

        override open func viewDidChangeEffectiveAppearance() {
            super.viewDidChangeEffectiveAppearance()
            updateColorScheme()
        }

        func updateColorScheme() {
            let scheme: TerminalColorScheme = switch effectiveAppearance.bestMatch(from: [.aqua, .darkAqua]) {
            case .darkAqua: .dark
            default: .light
            }
            surface?.setColorScheme(scheme.ghosttyValue)
            if let controller,
               let viewState = delegate as? TerminalViewState,
               viewState.controller === controller
            {
                viewState.adopt(terminalColorScheme: scheme)
            } else {
                controller?.setColorScheme(scheme)
            }
        }
    }
#endif
