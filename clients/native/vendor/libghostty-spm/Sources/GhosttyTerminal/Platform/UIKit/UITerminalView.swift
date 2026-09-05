//
//  UITerminalView.swift
//  libghostty-spm
//
//  Created by Lakr233 on 2026/3/16.
//

#if canImport(UIKit)
    import GhosttyKit
    import UIKit

    @MainActor
    open class UITerminalView: UIView {
        let core = TerminalSurfaceCoordinator()
        /// Pixel bounds last drawn synchronously from `layoutSubviews`.
        /// UIKit can resize the CAMetalLayer before the display link paints;
        /// tracking this prevents one old drawable from being stretched into
        /// the new terminal frame without blocking on every layout pass.
        var lastSynchronousLayoutPixelSize: CGSize = .zero
        var momentumDisplayLink: CADisplayLink?
        var momentumVelocity: CGPoint = .zero
        #if !targetEnvironment(macCatalyst)
            static let minFontSize: Float = 4
            static let maxFontSize: Float = 64
        #endif
        var activePointerButton: ghostty_input_mouse_button_e?
        var pointerSelectionStartPoint: CGPoint?
        var lastPointerSelectionRect: CGRect?
        var pendingSelectionMenuPoint: CGPoint?
        #if !targetEnvironment(macCatalyst)
            var indirectPointerPanOwnsTouchSequence = false
            var suppressNextIndirectPointerTouchEnd = false
            /// Modeless touch mouse, part 1: while true, a direct tap is a
            /// left CLICK at the touched cell instead of a keyboard focus
            /// toggle. Hosts push this from their knowledge of the remote
            /// program (mouse tracking enabled), so taps click exactly when
            /// something is listening; scrolling and all other gestures are
            /// unaffected.
            public var directTouchTapClickEnabled = false
            /// Fired after a tap-click's press/release is sent. The host
            /// owns the keyboard-focus decision (possibly asynchronously —
            /// e.g. waiting to see whether the remote caret follows the tap)
            /// and drives it through its ordinary focus binding; tap-clicks
            /// themselves never touch the responder chain.
            public var onPointerTapClick: ((CGPoint) -> Void)?
            /// Remote pinch: with the local font pinch disabled
            /// (`pinchZoomEnabled == false`), pinch steps land here instead —
            /// ± steps at the pinch centroid. Hosts forward them to the
            /// remote program (e.g. as ctrl+wheel, the terminal's
            /// conventional zoom axis).
            public var onPointerPinchZoom: ((Int, CGPoint) -> Void)?
            /// Modeless touch mouse, part 2: while true, press-and-hold
            /// picks up the mouse — the long-press recognizer drives
            /// press/drag/release for as long as the finger stays down
            /// (drag-select in place) — replacing the text-selection-sheet
            /// long-press. A fast drag still wins the pan recognizer first
            /// and scrolls; the two never fight.
            public var pointerLongPressDragEnabled = false
            /// Host callback for the active phase of that hold-drag. This is
            /// synchronous so an outer gesture (for example a navigation
            /// drawer) can stop claiming the same touch sequence as soon as
            /// text selection wins it.
            public var onPointerDragInProgressChange: ((Bool) -> Void)?
            /// True while a long-press mouse drag is in flight; hosts gate
            /// their own competing recognizers (canvas panning) on it.
            public internal(set) var pointerDragInProgress = false {
                didSet {
                    guard pointerDragInProgress != oldValue else { return }
                    onPointerDragInProgressChange?(pointerDragInProgress)
                }
            }
            weak var selectionLongPressRecognizer: UILongPressGestureRecognizer?
            var pointerTapCandidateLocation: CGPoint?
        #endif
        lazy var selectionContextMenuInteraction = UIContextMenuInteraction(delegate: self)
        var hardwareKeyHandled = false
        let touchScrollMultiplier: CGFloat = 2.6
        let touchMomentumMultiplier: CGFloat = 2.25
        let touchMomentumMinimumStartVelocity: CGFloat = 65
        let touchMomentumStopVelocity: CGFloat = 18
        let touchMomentumDecelerationPerFrameAt60Hz: CGFloat = 0.955
        #if !targetEnvironment(macCatalyst)
            var currentFontSize: Float = 14
            var lastPinchScale: CGFloat = 1.0
        #endif
        lazy var inputHandler = TerminalTextInputHandler(view: self)
        weak var _inputDelegate: (any UITextInputDelegate)?
        /// Fired from becomeFirstResponder/resignFirstResponder so hosts that
        /// mirror focus in their own state can track every dismissal path
        /// (accessory hide key, swipe-down, programmatic resign).
        public var onFocusChange: ((Bool) -> Void)?

        /// Taps on `.hostAction` accessory keys land here with the key's id
        /// (attach image, voice input, …). App-defined behavior.
        public var onAccessoryHostAction: ((String) -> Void)?

        /// The built-in pinch gesture changes the local FONT SIZE. Hosts
        /// whose layout math depends on stable cell metrics (remote
        /// fit-to-grid rendering) turn it off.
        public var pinchZoomEnabled = true

        /// Hides the system keyboard's dictation mic by reporting secure
        /// text entry (see `isSecureTextEntry` in the traits). For hosts
        /// that ship their own dictation. Set before the keyboard appears.
        public var systemDictationDisabled = false

        /// Keep the ghostty surface (terminal screen + scrollback state)
        /// alive while the view is detached from a window. Hosts that cache
        /// terminal views across SwiftUI teardowns re-attach with the
        /// previous content intact instead of a blank rebuild. Default
        /// false: the original behavior (detach frees the surface).
        public var retainsSurfaceWhenDetached = false

        #if !targetEnvironment(macCatalyst)
            lazy var terminalInputAccessory = TerminalInputAccessoryView(terminalView: self)
            let stickyModifiers = TerminalStickyModifierState()
            var softwareKeyboardVisible = false
            var pendingKeyboardDismissOnTouchEnd = false
            /// Focus is decided at touch END (like dismissal), so a scroll
            /// never summons the keyboard — only a clean tap does.
            var pendingKeyboardFocusOnTouchEnd = false
            var touchDidScrollDuringCurrentTouch = false
            var touchScrollDelegateOwnsSequence = false
        #endif

        #if !targetEnvironment(macCatalyst)
            open var inputAccessoryStyle: TerminalInputAccessoryStyle {
                get { terminalInputAccessory.style }
                set { terminalInputAccessory.style = newValue }
            }

            open var inputAccessoryItems: [TerminalInputAccessoryItem] = TerminalInputAccessoryItem.defaultItems {
                didSet {
                    terminalInputAccessory.rebuildContent()
                    reloadInputViews()
                }
            }

            /// Keys pinned at the bar's trailing edge, outside the scrolling
            /// key row — always visible (attach image, hide keyboard, …).
            open var inputAccessoryTrailingItems: [TerminalInputAccessoryItem] = [] {
                didSet {
                    terminalInputAccessory.rebuildContent()
                    reloadInputViews()
                }
            }
        #endif

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

        var surface: TerminalSurface? {
            core.surface
        }

        open var hasText: Bool {
            true
        }

        override open var canBecomeFirstResponder: Bool {
            true
        }

        override public init(frame: CGRect) {
            super.init(frame: frame)
            commonInit()
        }

        @available(*, unavailable)
        public required init?(coder _: NSCoder) {
            fatalError("init(coder:) has not been implemented")
        }

        func commonInit() {
            backgroundColor = .clear
            isOpaque = false
            isUserInteractionEnabled = true
            updateDisplayScale()

            core.isAttached = { [weak self] in self?.window != nil }
            core.scaleFactor = { [weak self] in
                Double(self?.resolvedDisplayScale() ?? UIScreen.main.nativeScale)
            }
            core.viewSize = { [weak self] in
                guard let self else { return (0, 0) }
                return (bounds.width, bounds.height)
            }
            core.platformSetup = { [weak self] config in
                guard let self else { return }
                config.platform_tag = GHOSTTY_PLATFORM_IOS
                config.platform = ghostty_platform_u(
                    ios: ghostty_platform_ios_s(
                        uiview: Unmanaged.passUnretained(self).toOpaque()
                    )
                )
            }
            core.onMetricsUpdate = { [weak self] in
                self?.updateSublayerFrames()
            }
            core.onCellSizeDidChange = { [weak self] in
                self?.refreshTextInputGeometry(reason: "cell-size-action")
            }
            core.onPostRender = { [weak self] in
                self?.enforceSublayerScale()
            }

            setupApplicationLifecycleObservers()
            syncApplicationActiveState()
            setupPlatformInput()
            #if !targetEnvironment(macCatalyst)
                setupKeyboardObservers()
            #endif
        }

        open func selectionMenuPoint(at point: CGPoint) -> CGPoint? {
            logPointerSelectionDiagnostics(
                context: "selectionMenuPoint",
                point: point
            )
            if let rect = lastPointerSelectionRect {
                let pointIsInsidePointerSelection = rect.insetBy(dx: -4, dy: -4).contains(point)
                guard pointIsInsidePointerSelection else {
                    TerminalDebugLog.log(
                        .input,
                        "selection menu miss point=\(NSCoder.string(for: point)) outside pointer selection"
                    )
                    return nil
                }
                guard surface?.hasSelection() == true else {
                    TerminalDebugLog.log(
                        .input,
                        "selection menu miss point=\(NSCoder.string(for: point)) inside pointer selection without active selection"
                    )
                    return nil
                }
                TerminalDebugLog.log(
                    .input,
                    "selection menu hit point=\(NSCoder.string(for: point)) inside pointer selection"
                )
                return point
            }

            guard surface?.hasSelection() == true else {
                TerminalDebugLog.log(
                    .input,
                    "selection menu miss point=\(NSCoder.string(for: point))"
                )
                return nil
            }

            guard surface?.selectionContainsQuicklookWord() == true else {
                TerminalDebugLog.log(
                    .input,
                    "selection menu miss point=\(NSCoder.string(for: point)) outside quicklook word"
                )
                return nil
            }

            TerminalDebugLog.log(
                .input,
                "selection menu hit point=\(NSCoder.string(for: point))"
            )
            return point
        }

        open func showSelectionCopyMenu(at point: CGPoint) {
            becomeFirstResponder()
            let menu = UIMenuController.shared
            menu.menuItems = nil
            menu.showMenu(
                from: self,
                rect: CGRect(x: point.x, y: point.y, width: 1, height: 1)
            )
            menu.update()
        }

        @discardableResult
        open func copySelectedTextToPasteboard() -> Bool {
            #if DEBUG
                if ProcessInfo.processInfo.arguments.contains("--ui-testing") {
                    accessibilityValue = nil
                }
            #endif
            guard let text = surface?.readSelection(), !text.isEmpty else {
                return false
            }
            UIPasteboard.general.string = text
            #if DEBUG
                if ProcessInfo.processInfo.arguments.contains("--ui-testing") {
                    accessibilityValue = text
                }
            #endif
            TerminalDebugLog.log(
                .input,
                "selection copied bytes=\(text.utf8.count) lines=\(TerminalInputText.lineCount(in: text))"
            )
            return true
        }

        open func selectionContextMenuConfiguration(
            at _: CGPoint
        ) -> UIContextMenuConfiguration {
            UIContextMenuConfiguration(identifier: nil, previewProvider: nil) { [weak self] _ in
                UIMenu(children: self?.selectionContextMenuElements() ?? [])
            }
        }

        open func selectionContextMenuElements() -> [UIMenuElement] {
            let copy = UIAction(
                title: "Copy",
                image: UIImage(systemName: "doc.on.doc")
            ) { [weak self] _ in
                self?.copySelectedTextToPasteboard()
            }
            return [copy]
        }

        deinit {
            NotificationCenter.default.removeObserver(self)
        }

        #if !targetEnvironment(macCatalyst)
            func setupKeyboardObservers() {
                NotificationCenter.default.addObserver(
                    self,
                    selector: #selector(keyboardDidShow),
                    name: UIResponder.keyboardDidShowNotification,
                    object: nil
                )
                NotificationCenter.default.addObserver(
                    self,
                    selector: #selector(keyboardDidHide),
                    name: UIResponder.keyboardDidHideNotification,
                    object: nil
                )
            }

            @objc func keyboardDidShow(_: Notification) {
                guard isFirstResponder else { return }
                softwareKeyboardVisible = true
            }

            @objc func keyboardDidHide(_: Notification) {
                softwareKeyboardVisible = false
            }
        #endif

        func refreshTextInputGeometry(reason: String) {
            guard isFirstResponder || inputHandler.hasMarkedText else { return }
            TerminalDebugLog.log(.ime, "refresh text geometry reason=\(reason)")
            inputHandler.notifyGeometryDidChange(reason: reason)
        }

        func refreshInputAccessoryContent() {
            #if !targetEnvironment(macCatalyst)
                terminalInputAccessory.refreshContent()
            #endif
        }
    }
#endif
