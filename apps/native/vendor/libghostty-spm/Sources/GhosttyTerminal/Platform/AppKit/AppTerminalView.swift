//
//  AppTerminalView.swift
//  libghostty-spm
//
//  Created by Lakr233 on 2026/3/16.
//

#if canImport(AppKit) && !canImport(UIKit)
    import AppKit
    import GhosttyKit

    @MainActor
    open class AppTerminalView: NSView {
        let core = TerminalSurfaceCoordinator()
        var metalLayer: CAMetalLayer?
        /// Last real window scale, so detached layouts never adopt another
        /// display's DPI (see core.scaleFactor).
        var lastWindowBackingScale: CGFloat?
        var inputHandler: TerminalKeyEventHandler?
        var lastPerformKeyEvent: TimeInterval?
        var pointerSelectionStartPoint: CGPoint?
        var lastPointerSelectionRect: CGRect?
        var pendingSelectionMenuPoint: CGPoint?
        /// A cmd-click that opened a file path is consumed: the matching
        /// mouseUp must not send a stray button release / start a selection.
        var commandClickConsumed = false
        var onFocusChange: ((Bool) -> Void)?

        open weak var delegate: (any TerminalSurfaceViewDelegate)? {
            get { core.delegate }
            set { core.delegate = newValue }
        }

        open var controller: TerminalController? {
            get { core.controller }
            set { core.controller = newValue }
        }

        open var configuration: TerminalSurfaceOptions {
            get { core.configuration }
            set { core.configuration = newValue }
        }

        open func setSurfaceVisible(_ visible: Bool) {
            core.setDisplayVisible(visible)
        }

        /// Renders a frame synchronously when the surface is visible and
        /// attached. Hosts that retain terminal views and swap them in and
        /// out of the hierarchy call this right after attaching so the same
        /// CATransaction that performs the swap already presents a current
        /// frame; the regular wakeup path defers the first draw to the next
        /// runloop turn, which shows the stale drawable from before the
        /// view was detached for a frame or two.
        open func renderImmediately() {
            core.renderImmediately()
        }

        /// Like `renderImmediately`, but forces the draw itself to happen
        /// synchronously even when the pixel size is unchanged (see
        /// `TerminalSurfaceCoordinator.renderImmediately(synchronousDraw:)`).
        /// Use on adoption swaps, where the retained view's layer holds a
        /// drawable from before it was hidden.
        open func renderImmediately(synchronousDraw: Bool) {
            core.renderImmediately(synchronousDraw: synchronousDraw)
        }

        var surface: TerminalSurface? {
            core.surface
        }

        /// Top-left corner of the cell grid in view points (top-left-origin
        /// space), for point→cell mapping. Hosts running with
        /// `window-padding-balance = false` set this to their fixed padding;
        /// nil keeps the legacy centered-grid assumption.
        public var gridOrigin: CGPoint?

        override public init(frame: NSRect) {
            super.init(frame: frame)
            commonInit()
        }

        @available(*, unavailable)
        public required init?(coder _: NSCoder) {
            fatalError("init(coder:) has not been implemented")
        }

        func commonInit() {
            wantsLayer = true

            let metal = CAMetalLayer()
            metal.device = MTLCreateSystemDefaultDevice()
            metal.pixelFormat = .bgra8Unorm
            metal.framebufferOnly = true
            metal.contentsScale = NSScreen.main?.backingScaleFactor ?? 2.0
            metal.isOpaque = false
            metal.backgroundColor = NSColor.clear.cgColor
            // During a live resize there is a gap between the layer taking
            // its new bounds and the next drawable being presented. The
            // default gravity (.resize) STRETCHES the stale drawable to the
            // new bounds for that gap — visible as the whole grid smearing
            // for a frame before it settles. Top-left gravity keeps the old
            // content pinned at its true size instead; the fresh draw lands
            // right behind it (setFrameSize renders synchronously).
            metal.contentsGravity = .topLeft
            layer = metal
            metalLayer = metal
            layer?.backgroundColor = NSColor.clear.cgColor

            inputHandler = TerminalKeyEventHandler(view: self)
            setupTrackingArea()

            core.isAttached = { [weak self] in self?.window != nil }
            core.scaleFactor = { [weak self] in
                // Detached views keep their LAST window's scale: falling
                // back to NSScreen.main mid-reparent can flip a 2x surface
                // to a 1x main display's scale for one fit, halving the
                // reported grid (the remote-PTY shrink bug).
                if let scale = self?.window?.backingScaleFactor {
                    self?.lastWindowBackingScale = scale
                    return Double(scale)
                }
                return Double(
                    self?.lastWindowBackingScale
                        ?? NSScreen.main?.backingScaleFactor ?? 2.0
                )
            }
            core.viewSize = { [weak self] in
                guard let self else { return (0, 0) }
                return (bounds.width, bounds.height)
            }
            core.currentDisplayID = { [weak self] in
                guard let screen = self?.window?.screen,
                      let number = screen.deviceDescription[
                          NSDeviceDescriptionKey("NSScreenNumber")
                      ] as? NSNumber
                else { return nil }
                return number.uint32Value
            }
            core.platformSetup = { [weak self] config in
                guard let self else { return }
                config.platform_tag = GHOSTTY_PLATFORM_MACOS
                config.platform = ghostty_platform_u(
                    macos: ghostty_platform_macos_s(
                        nsview: Unmanaged.passUnretained(self).toOpaque()
                    )
                )
            }
            core.onMetricsUpdate = { [weak self] in
                self?.updateMetalLayerMetrics()
            }
            core.onPostRender = { [weak self] in
                self?.enforceMetalLayerScale()
            }
        }

        open func selectionMenuPoint(at point: CGPoint) -> CGPoint? {
            guard surface?.hasSelection() == true else {
                TerminalDebugLog.log(
                    .input,
                    "selection menu miss point=\(selectionPointDescription(point))"
                )
                return nil
            }

            if let rect = lastPointerSelectionRect {
                guard rect.insetBy(dx: -4, dy: -4).contains(point) else {
                    TerminalDebugLog.log(
                        .input,
                        "selection menu miss point=\(selectionPointDescription(point)) outside pointer selection"
                    )
                    return nil
                }

                TerminalDebugLog.log(
                    .input,
                    "selection menu hit point=\(selectionPointDescription(point)) inside pointer selection"
                )
                return point
            }

            guard surface?.selectionContainsQuicklookWord() == true else {
                TerminalDebugLog.log(
                    .input,
                    "selection menu miss point=\(selectionPointDescription(point)) outside quicklook word"
                )
                return nil
            }

            TerminalDebugLog.log(
                .input,
                "selection menu hit point=\(selectionPointDescription(point))"
            )
            return point
        }

        open func selectionContextMenu() -> NSMenu {
            let menu = NSMenu()
            let copyItem = NSMenuItem(
                title: "Copy",
                action: #selector(copy(_:)),
                keyEquivalent: ""
            )
            copyItem.target = self
            menu.addItem(copyItem)
            return menu
        }

        @discardableResult
        open func copySelectedTextToPasteboard() -> Bool {
            guard let text = surface?.readSelection(), !text.isEmpty else {
                return false
            }
            let pasteboard = NSPasteboard.general
            pasteboard.clearContents()
            pasteboard.setString(text, forType: .string)
            TerminalDebugLog.log(
                .input,
                "selection copied bytes=\(text.utf8.count) lines=\(TerminalInputText.lineCount(in: text))"
            )
            return true
        }

        private func selectionPointDescription(_ point: CGPoint) -> String {
            "\(String(format: "%.2f", point.x))x\(String(format: "%.2f", point.y))"
        }

        deinit {
            NotificationCenter.default.removeObserver(self)
        }
    }
#endif
