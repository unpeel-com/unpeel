//
//  GhosttyBridge.swift
//  UnpeelNative
//
//  The ONLY file in this target allowed to import GhosttyTerminal /
//  GhosttyKit (PRD §8: libghostty API is alpha; churn must be contained
//  here). Everything Ghostty-shaped is translated into plain AppKit /
//  Foundation types at this boundary.
//
//  What this bridge does:
//  - Owns a `TerminalController` (one ghostty_app_t + config per pane).
//  - Sets the surface command via the Ghostty config `command` key.
//    (The C API also supports per-surface `command` / `env_vars` on
//    `ghostty_surface_config_s`, but the GhosttyTerminal wrapper does not
//    expose those yet; one controller per pane gives us per-session
//    commands today. See report/PRD §11.1.)
//  - Hosts the wrapper's `TerminalView` (AppKit NSView, Metal-rendered,
//    full key/mouse/IME pipeline) with the EXEC io backend, so Ghostty
//    owns a real PTY running our command — exactly Strategy A.
//
//  Runtime callbacks: wakeup, clipboard read/write/confirm, config
//  reload and the action dispatch loop are implemented inside
//  GhosttyTerminal's TerminalController+Callbacks. We only consume the
//  high-level delegate protocols below; unhandled actions are logged by
//  the wrapper when debug logging is enabled.
//

import AppKit
import GhosttyKit
import GhosttyTerminal
import SwiftUI

/// Plain-Swift events the rest of the app may care about. No Ghostty types.
@MainActor
protocol GhosttyTerminalPaneDelegate: AnyObject {
    func terminalPane(_ pane: GhosttyTerminalPane, didChangeTitle title: String)
    /// The surface's child process exited (or the surface closed).
    func terminalPane(_ pane: GhosttyTerminalPane, didCloseProcessAlive processAlive: Bool)
}

/// The exec-backed local terminal can turn a path region published by a TUI
/// (or an explicit OSC 8 `file://` fallback) into a native macOS drag.
/// Delaying the underlying terminal mouse-down until mouse-up preserves an
/// ordinary click, while crossing the drag threshold keeps the TUI from
/// receiving half of a gesture that AppKit now owns.
///
/// Remote terminal panes intentionally keep using the plain `TerminalView`:
/// a Host path must never be advertised as a Controller-local file URL.
@MainActor
private final class PathDraggableTerminalView: TerminalView, NSDraggingSource {
    var pathAtCell: ((_ row: Int, _ column: Int, _ rowText: String) -> String?)?

    private struct Candidate {
        let path: String
        let event: NSEvent
        let start: CGPoint
    }

    private var candidate: Candidate?
    private var pathDragInFlight = false
    private static let dragThreshold: CGFloat = 4

    override func mouseDown(with event: NSEvent) {
        guard event.buttonNumber == 0,
              let hit = viewportRowHit(at: event),
              let path = pathAtCell?(hit.row, hit.column, hit.text)
        else {
            candidate = nil
            super.mouseDown(with: event)
            return
        }

        window?.makeFirstResponder(self)
        candidate = Candidate(
            path: path,
            event: event,
            start: convert(event.locationInWindow, from: nil)
        )
    }

    override func mouseDragged(with event: NSEvent) {
        if pathDragInFlight { return }
        guard let candidate else {
            super.mouseDragged(with: event)
            return
        }

        let point = convert(event.locationInWindow, from: nil)
        guard hypot(point.x - candidate.start.x, point.y - candidate.start.y)
            >= Self.dragThreshold
        else { return }

        self.candidate = nil
        pathDragInFlight = beginPathDrag(candidate.path, with: event, at: point)
    }

    override func mouseUp(with event: NSEvent) {
        if pathDragInFlight { return }
        guard let candidate else {
            super.mouseUp(with: event)
            return
        }

        self.candidate = nil
        // The press was held back in case this became an AppKit drag. Replay
        // the complete click now that it did not, so drag-enabled rows remain
        // ordinary terminal content for clicks and text selection.
        super.mouseDown(with: candidate.event)
        super.mouseUp(with: event)
    }

    private func beginPathDrag(_ path: String, with event: NSEvent, at point: CGPoint) -> Bool {
        let fileURL = URL(fileURLWithPath: path)
        let pasteboardItem = NSPasteboardItem()
        guard pasteboardItem.setString(fileURL.absoluteString, forType: .fileURL) else {
            return false
        }
        // Plain text is an interoperability fallback for destinations that do
        // not consume file URLs. Unpeel's terminal target prefers `.fileURL`
        // and applies its existing shell quoting + bracketed-paste path.
        pasteboardItem.setString(path, forType: .string)

        let item = NSDraggingItem(pasteboardWriter: pasteboardItem)
        let size = CGSize(width: 36, height: 36)
        let frame = CGRect(
            x: point.x - size.width / 2,
            y: point.y - size.height / 2,
            width: size.width,
            height: size.height
        )
        let icon = (NSWorkspace.shared.icon(forFile: path).copy() as? NSImage)
            ?? NSWorkspace.shared.icon(forFile: path)
        icon.size = size
        item.setDraggingFrame(frame, contents: icon)

        let session = beginDraggingSession(with: [item], event: event, source: self)
        session.animatesToStartingPositionsOnCancelOrFail = true
        return true
    }

    func draggingSession(
        _: NSDraggingSession,
        sourceOperationMaskFor _: NSDraggingContext
    ) -> NSDragOperation {
        .copy
    }

    func draggingSession(
        _: NSDraggingSession,
        endedAt _: NSPoint,
        operation _: NSDragOperation
    ) {
        pathDragInFlight = false
    }

    func ignoreModifierKeys(for _: NSDraggingSession) -> Bool {
        true
    }
}

/// An NSView containing one GPU-rendered Ghostty terminal surface running
/// a fixed command. Fills itself with the surface; resize is handled by
/// AppKit layout + the wrapper's `fitToSize()`.
@MainActor
final class GhosttyTerminalPane: NSView {
    weak var paneDelegate: GhosttyTerminalPaneDelegate?
    var commandClickHandler: ((ClickablePath.Match, String) -> Bool)?

    private let terminalView: PathDraggableTerminalView
    private let controller: TerminalController
    private var surface: TerminalSurface?
    private var didTearDown = false

    /// Most recent working directory, used to resolve relative file paths from
    /// cmd-click. Seeded with the spawn cwd; updated by OSC 7 if the shell
    /// reports it.
    private var currentWorkingDirectory: String?

    /// Stable launch root used to make dropped paths concise. Unlike
    /// `currentWorkingDirectory`, this does not follow an agent that `cd`s
    /// into a child folder: paths inside the Session's project remain rooted
    /// at the project boundary the user recognizes in Unpeel.
    private let projectRootDirectory: String?

    /// Local hosted-session directory containing transient App presentation
    /// markers such as `terminal-drag-map.json`. nil for non-hosted/spike
    /// surfaces; remote panes use a separate type and never receive this.
    private let sessionDirectory: URL?

    /// Native file drags can be routed to a semantic destination published by
    /// the hosted TUI (for example Markdown's editing surface). Hover writes
    /// are throttled but intentionally repeat at a stationary edge so the TUI
    /// can continue auto-scrolling.
    private var appDropTargetActive = false
    private var lastAppDropHoverCell: (row: Int, column: Int)?
    private var lastAppDropHoverAt: TimeInterval = 0
    private static let appDropHoverInterval: TimeInterval = 0.075

    /// Ghostty reports the semantic target beneath the pointer for explicit
    /// links. Only a local `file://` URL can arm the native drag source.
    private var hoveredLink: String?

    /// Scroll-to-bottom overlay (bottom-right, like the Tauri app). The
    /// pane owns it so per-session scroll state survives surface swaps in
    /// the cache. Visibility is driven by Ghostty's scrollbar action via
    /// `TerminalSurfaceScrollbarDelegate`, plus the TUI jump-hint scan below.
    private let scrollButtonModel = TerminalScrollButtonModel()

    /// True while Ghostty reports the viewport above the scrollback tail.
    private var scrolledUpInScrollback = false

    // MARK: - Full-screen TUI "jump to bottom" hint

    /// Claude Code's TUI keeps its own virtual scroll (drawn in the primary
    /// screen, repainted in place), so the terminal never reports
    /// "scrolled up" and the overlay button never shows. The TUI's own signal
    /// is a hint drawn on screen. While the pane is visible we scan the
    /// rendered viewport for that hint; when present the same overlay
    /// button appears, and pressing it fakes the ctrl+End keypress
    /// (CSI 1;5F — the bytes a real ctrl+End sends) instead of moving the
    /// viewport.
    ///
    /// Matches the hint chip in its two shapes — "Jump to bottom
    /// (ctrl+End)" and "3 new messages (ctrl+End) ↓" — and nothing looser,
    /// and only within the bottom rows of the viewport, where Claude pins
    /// the real chip (just above its composer). Quoted hint text higher up
    /// in the transcript must not match: a spurious ctrl+End at a TUI that
    /// is not scrolled up lands as literal "[1;5F" junk in the composer.
    /// Keep aligned with the iOS matcher in
    /// `RemoteGhosttyTerminalView.swift`.
    private static func viewportHasTuiJumpHint(_ text: String) -> Bool {
        let tail = text
            .split(separator: "\n", omittingEmptySubsequences: false)
            .suffix(15)
            .joined(separator: "\n")
        if tail.contains("Jump to bottom (ctrl+End)") { return true }
        return tail.firstMatch(of: /\d+ new messages? \(ctrl\+End\)/) != nil
    }
    /// macOS virtual keycode for End (kVK_End). The jump is sent as a real
    /// ctrl+End KEY EVENT, not raw CSI bytes: `sendText` bypasses ghostty's
    /// key encoder, so TUIs that negotiated the kitty keyboard protocol
    /// (Claude Code) received mangled text — a literal "[1;5F" in the
    /// composer — instead of the keypress.
    private static let endKeycode: UInt32 = 119
    private var tuiJumpHintActive = false
    private var tuiJumpHintTimer: Timer?
    /// Armed by a button press: the remote TUI repaints asynchronously, so
    /// one ctrl+End can land mid-stream and leave the hint up (users had to
    /// click repeatedly). While retries remain, the 0.5s hint poll re-sends
    /// the key instead of re-showing the button; a clean scan settles it.
    private var tuiJumpRetriesRemaining = 0
    /// Last scrollbar metrics (drives the scrolled-up state of the shared
    /// overlay button).
    private var lastScrollbarMetrics: TerminalScrollbarMetrics?

    // MARK: - Find (⌘F)

    /// Lazily built ⌘F bar (top-right overlay). libghostty runs the search;
    /// the pane relays query/navigation as binding actions and match counts
    /// back via `TerminalSurfaceSearchDelegate`. Owned per-pane so a
    /// session's find state survives surface-cache swaps like scroll state
    /// does.
    private var findBar: TerminalFindBar?
    private var findBarVisible = false
    private var searchTotal: Int?
    private var searchSelected: Int?
    private var findObservers: [NSObjectProtocol] = []

    /// Ghostty `background-opacity` currently rendered by this pane's
    /// controller; applyPaneStyle pushes a live config overlay only when the
    /// Appearance setting actually moved (a full config apply is expensive).
    private var appliedBackgroundOpacity: Double = 1

    /// Corner container for the overlay button: passes clicks through to
    /// the terminal whenever the button is not showing.
    private final class ScrollButtonContainer: NSView {
        var isInteractive: () -> Bool = { false }
        override func hitTest(_ point: NSPoint) -> NSView? {
            isInteractive() ? super.hitTest(point) : nil
        }
    }

    /// - Parameters:
    ///   - command: command line the surface executes (Ghostty splits args
    ///     itself; e.g. "/bin/zsh --login" or "unpeel-attach <session-id>").
    ///   - workingDirectory: initial cwd for the spawned process.
    ///   - style: plain-Swift terminal theme (DESIGN.md §3); translated into
    ///     Ghostty config keys here, at the bridge boundary.
    init(
        command: String,
        workingDirectory: String?,
        sessionDirectory: URL? = nil,
        style: TerminalPaneStyle = .resolved()
    ) {
        currentWorkingDirectory = workingDirectory
        projectRootDirectory = workingDirectory.map {
            ($0 as NSString).standardizingPath
        }
        self.sessionDirectory = sessionDirectory
        // Per-pane controller = per-pane ghostty config = per-pane command.
        // Colors live in the TerminalTheme, NOT the base config: the wrapper
        // re-resolves the active variant whenever the view's effective
        // appearance changes (viewDidChangeEffectiveAppearance →
        // controller.setColorScheme), which is how the surface follows the
        // app's light/dark mode without rebuilding the pane.
        controller = TerminalController(theme: Self.terminalTheme(for: style)) { builder in
            builder.withCustom("command", command)
            // Don't keep dead surfaces around.
            builder.withCustom("wait-after-command", "false")
            builder.withCustom("shell-integration", "detect")
            Self.applySurfaceKeybinds(&builder)
            // The Swift frame bleeds to the window edges; per-provider
            // terminal styles decide whether Ghostty keeps any cell padding.
            builder.withCustom("window-padding-x", "\(style.windowPaddingX)")
            builder.withCustom("window-padding-y", "\(style.windowPaddingY)")
            builder.withCustom(
                "window-padding-balance",
                style.windowPaddingBalanced ? "true" : "false"
            )
            // Extend the terminal background into padding so empty rows /
            // residual padding match the TUI canvas (OpenCode/Grok) instead
            // of leaving a mismatched strip of the default theme.
            builder.withCustom("window-padding-color", "extend")
            // Discrete (wheel-tick) speed only. Precision must stay 1: the
            // multiplier scales trackpad deltas BEFORE mouse-report
            // conversion, so anything >1 makes mouse-captured TUIs (Claude's
            // virtual scroll) jump multiple lines per finger-travel line —
            // the "not as smooth as Ghostty" complaint. A bare value would
            // set both fields.
            builder.withCustom(
                "mouse-scroll-multiplier",
                "precision:1,discrete:\(style.mouseScrollMultiplier)"
            )

            builder.withCursorStyle(.block)
            builder.withCursorStyleBlink(true)
            builder.withFontSize(style.fontSize)
            if let family = style.fontFamily {
                builder.withFontFamily(family)
            }
            // Settings ▸ Appearance transparency. Text stays fully opaque;
            // only the canvas (and extended padding) picks up the alpha.
            builder.withBackgroundOpacity(style.backgroundOpacity)
        }
        appliedBackgroundOpacity = style.backgroundOpacity

        // A config-build failure leaves the controller without a ghostty
        // app, and every later surface rebuild fails with no explanation —
        // surface it loudly instead.
        if let issue = controller.lastConfigurationIssue {
            NSLog("[UnpeelNative] ghostty configuration issue: %@", issue)
        }

        terminalView = PathDraggableTerminalView(frame: .zero)
        // Unbalanced padding pins the grid at the fixed top-left padding —
        // give the view the exact origin for point→cell mapping (cmd-click).
        terminalView.gridOrigin = CGPoint(
            x: CGFloat(style.windowPaddingX),
            y: CGFloat(style.windowPaddingY)
        )

        super.init(frame: .zero)

        terminalView.pathAtCell = { [weak self] row, column, rowText in
            self?.draggablePath(atScreenRow: row, column: column, rowText: rowText)
        }

        // Metal surface is clear; paint this pane opaque with the frame bg so
        // any gap around the surface (letterbox, padding, resize) matches the
        // TUI canvas rather than showing through to the content dim.
        applyFrameLayerBackground(style)

        terminalView.configuration = TerminalSurfaceOptions(
            backend: .exec,
            workingDirectory: workingDirectory,
            context: .window
        )
        terminalView.delegate = self
        terminalView.controller = controller

        terminalView.translatesAutoresizingMaskIntoConstraints = false
        addSubview(terminalView)
        NSLayoutConstraint.activate([
            terminalView.topAnchor.constraint(equalTo: topAnchor),
            terminalView.leadingAnchor.constraint(equalTo: leadingAnchor),
            terminalView.trailingAnchor.constraint(equalTo: trailingAnchor),
            terminalView.bottomAnchor.constraint(equalTo: bottomAnchor),
        ])

        // The wrapper's TerminalView registers no drag types, so file/image
        // drags over the surface fall through to this pane.
        registerForDraggedTypes(Self.dropPasteboardTypes)

        scrollButtonModel.action = { [weak self] in self?.scrollButtonPressed() }
        let host = NSHostingView(
            rootView: TerminalScrollToBottomButton(model: scrollButtonModel)
        )
        host.translatesAutoresizingMaskIntoConstraints = false
        let buttonContainer = ScrollButtonContainer()
        buttonContainer.isInteractive = { [scrollButtonModel] in
            scrollButtonModel.visible
        }
        buttonContainer.translatesAutoresizingMaskIntoConstraints = false
        buttonContainer.addSubview(host)
        addSubview(buttonContainer, positioned: .above, relativeTo: terminalView)
        NSLayoutConstraint.activate([
            host.topAnchor.constraint(equalTo: buttonContainer.topAnchor),
            host.leadingAnchor.constraint(equalTo: buttonContainer.leadingAnchor),
            host.trailingAnchor.constraint(equalTo: buttonContainer.trailingAnchor),
            host.bottomAnchor.constraint(equalTo: buttonContainer.bottomAnchor),
            // The SwiftUI root carries 8pt of padding (animation headroom),
            // so -12/-8 here lands the 36pt button at 20/16 from the corner,
            // matching the Tauri app.
            buttonContainer.trailingAnchor.constraint(equalTo: trailingAnchor, constant: -12),
            buttonContainer.bottomAnchor.constraint(equalTo: bottomAnchor, constant: -8),
        ])

        // Edit ▸ Find menu commands. Every retained pane hears the post;
        // only the one actually on screen (in a window, not inside a hidden
        // warm-pane container, key window) acts.
        let center = NotificationCenter.default
        findObservers = [
            center.addObserver(
                forName: .unpeelTerminalFind, object: nil, queue: .main
            ) { [weak self] _ in
                MainActor.assumeIsolated {
                    guard let self, self.isDisplayedForFind else { return }
                    self.showFindBar()
                }
            },
            center.addObserver(
                forName: .unpeelTerminalFindNext, object: nil, queue: .main
            ) { [weak self] _ in
                MainActor.assumeIsolated {
                    guard let self, self.isDisplayedForFind else { return }
                    self.findNext()
                }
            },
            center.addObserver(
                forName: .unpeelTerminalFindPrevious, object: nil, queue: .main
            ) { [weak self] _ in
                MainActor.assumeIsolated {
                    guard let self, self.isDisplayedForFind else { return }
                    self.findPrevious()
                }
            },
        ]
    }

    @available(*, unavailable)
    required init?(coder _: NSCoder) { fatalError("not supported") }

    /// Push a new frame style into the live Ghostty controller (colors only).
    /// Used when OpenCode/Grok config changes while a pane is retained — no
    /// surface rebuild. Window padding stays at whatever was set at create
    /// (provider panes already zero it). Also updates this pane's opaque
    /// layer so letterbox/padding matches the new canvas.
    func applyPaneStyle(_ style: TerminalPaneStyle) {
        _ = controller.setTheme(Self.terminalTheme(for: style))
        if style.backgroundOpacity != appliedBackgroundOpacity {
            appliedBackgroundOpacity = style.backgroundOpacity
            // Per-session override on top of the base config; the wrapper
            // pushes it to the live surface (ghostty_surface_update_config).
            _ = controller.setTerminalConfiguration(
                TerminalConfiguration {
                    $0.withBackgroundOpacity(style.backgroundOpacity)
                }
            )
        }
        applyFrameLayerBackground(style)
    }

    /// Explicitly detach/free the Ghostty surface before a cache eviction
    /// releases this pane. `TerminalView.controller = nil` is the wrapper's
    /// supported teardown path: its coordinator clears callbacks and queues
    /// `ghostty_surface_free`, which terminates the EXEC child
    /// (`unpeel-attach`) without waiting for ARC/deinit timing. Idempotent so
    /// a delayed final release remains harmless.
    func tearDown() {
        guard !didTearDown else { return }
        didTearDown = true
        stopTuiJumpHintPolling()
        terminalView.setSurfaceVisible(false)
        // Keep the delegate installed for the synchronous detach callback so
        // our cached `surface` pointer is cleared before severing the bridge.
        terminalView.controller = nil
        terminalView.delegate = nil
        paneDelegate = nil
    }

    private func applyFrameLayerBackground(_ style: TerminalPaneStyle) {
        wantsLayer = true
        // Translucent terminals: the surface's own background-opacity is the
        // only canvas paint; an opaque layer here would sit under the clear
        // Metal surface and cancel the transparency.
        if style.backgroundOpacity < 1 {
            layer?.backgroundColor = NSColor.clear.cgColor
            return
        }
        // Match the currently effective appearance so light/dark configs
        // don't leave the wrong fixed layer color under a clear Metal surface.
        let appearance = window?.effectiveAppearance ?? effectiveAppearance
        let isDark = appearance.bestMatch(from: [.aqua, .darkAqua]) == .darkAqua
        let hex = isDark ? style.dark.background : style.light.background
        layer?.backgroundColor = (Self.nsColor(fromHexString: hex) ?? Theme.terminalBackgroundNSColor)
            .cgColor
    }

    fileprivate static func nsColor(fromHexString value: String) -> NSColor? {
        var body = value.trimmingCharacters(in: .whitespacesAndNewlines)
        if body.hasPrefix("#") { body.removeFirst() }
        guard body.count == 6, let rgb = UInt32(body, radix: 16) else { return nil }
        return NSColor(hex: rgb)
    }

    /// libghostty ships ~92 default keybinds and a focused surface consumes
    /// any chord it has a binding for BEFORE NSMenu key equivalents run
    /// (AppTerminalView.performKeyEquivalent → ghostty_surface_key_is_binding)
    /// — so defaults like super+w=close_surface silently eat app chords
    /// (⌘W Close Window, ⌘N, ⌘Q…). Clear them all; NSMenu is the single
    /// owner of app chords. The surface keeps only chords that must act on
    /// the terminal itself, re-added explicitly below. Font zoom binds
    /// translated characters only (⌘= / ⌘+ / ⌘−), so layouts with a
    /// dedicated plus key work without physical-key binds stealing them.
    /// Copy stays
    /// `performable` (consumed only when a selection exists), matching
    /// ghostty's own default, so a bare ⌘C over an empty terminal still
    /// reaches the Edit menu. Paste is `performable` too, again matching
    /// ghostty: the clipboard read callback returns false when the
    /// pasteboard holds no text (an image, say), the binding is then not
    /// performed, and ⌘V is encoded as an ordinary key event — so an agent
    /// that negotiated the kitty keyboard protocol receives super+v and
    /// can run its own clipboard-image paste, exactly like Ctrl+V.
    fileprivate static func applySurfaceKeybinds(
        _ builder: inout TerminalConfiguration.Builder
    ) {
        builder.withCustom("keybind", "clear")
        for keybind in surfaceKeybinds {
            builder.withCustom("keybind", keybind)
        }
    }

    /// Explicit subset of Ghostty's macOS defaults retained after `clear`.
    /// Internal so the app test suite can keep essential terminal shortcuts
    /// from silently disappearing again.
    static let surfaceKeybinds = [
        "performable:super+c=copy_to_clipboard",
        "performable:super+v=paste_from_clipboard",
        // Font zoom binds by unicode codepoint only, mirroring Ghostty's own
        // defaults ("equal"/"minus" would bind the PHYSICAL keys, which win
        // over codepoint matches — on e.g. Norwegian layouts the dedicated
        // "+" key sits on physical Minus, turning ⌘+ into zoom-out).
        "super+plus=increase_font_size:1",
        "super+==increase_font_size:1",
        "super+-=decrease_font_size:1",
        "super+zero=reset_font_size",
        // Scrollback navigation (same as Ghostty's macOS defaults).
        "super+home=scroll_to_top",
        "super+end=scroll_to_bottom",
        "super+page_up=scroll_page_up",
        "super+page_down=scroll_page_down",
    ]

    /// TerminalPaneStyle variants → the wrapper's light/dark TerminalTheme
    /// (matches the Svelte app's xterm themes exactly).
    fileprivate static func terminalTheme(for style: TerminalPaneStyle) -> TerminalTheme {
        TerminalTheme(
            light: themeConfiguration(style.light),
            dark: themeConfiguration(style.dark)
        )
    }

    private static func themeConfiguration(
        _ variant: TerminalPaneStyle.Variant
    ) -> TerminalConfiguration {
        TerminalConfiguration { builder in
            builder.withBackground(variant.background)
            builder.withForeground(variant.foreground)
            builder.withSelectionBackground(variant.selectionBackground)
            builder.withCursorColor(variant.cursorColor)
            for (index, color) in variant.palette.enumerated() {
                builder.withPalette(index, color: color)
            }
        }
    }

    deinit {
        MainActor.assumeIsolated {
            if let observer = occlusionObserver {
                NotificationCenter.default.removeObserver(observer)
            }
            for observer in findObservers {
                NotificationCenter.default.removeObserver(observer)
            }
            tuiJumpHintTimer?.invalidate()
        }
    }

    // MARK: - Render pause for hidden surfaces

    /// Retained-but-detached panes (SurfaceCache keeps every live session's
    /// pane alive; only the selected one is in the view hierarchy) and panes
    /// in an occluded/minimized window must not keep Ghostty's renderer
    /// drawing frames nobody sees. The wrapper exposes this as
    /// `TerminalView.setSurfaceVisible(_:)` → `ghostty_surface_set_occlusion`
    /// plus suspension of its wakeup→tick→draw loop, mirroring what Ghostty
    /// itself does on `NSWindow.occlusionState` changes.
    private var occlusionObserver: NSObjectProtocol?
    var onPresentationVisibilityChanged: (() -> Void)?
    private var lastThemeSamplingVisibility = false
    var isPresentedForThemeSampling: Bool {
        window?.occlusionState.contains(.visible) == true && !isHiddenOrHasHiddenAncestor
    }

    // NOTE for pre-warmed panes (WarmPaneHostView): they are mounted inside
    // a HIDDEN container but must NOT be paused via setSurfaceVisible(false)
    // — that suspends the wrapper's wakeup→tick loop, and a surface that
    // never ticks while its attach client floods the replay wedges its IO;
    // the next synchronous surface call from the main thread (adoption on
    // click) then deadlocks. Hidden-but-ticking is the safe state; the
    // hidden container already keeps them out of the compositor.
    override func viewDidMoveToWindow() {
        super.viewDidMoveToWindow()
        if let observer = occlusionObserver {
            NotificationCenter.default.removeObserver(observer)
            occlusionObserver = nil
        }
        if let window {
            occlusionObserver = NotificationCenter.default.addObserver(
                forName: NSWindow.didChangeOcclusionStateNotification,
                object: window,
                queue: .main
            ) { [weak self] _ in
                MainActor.assumeIsolated {
                    self?.updateSurfaceVisibility()
                }
            }
        }
        updateSurfaceVisibility()
    }

    private func updateSurfaceVisibility() {
        let presented = isPresentedForThemeSampling
        if presented != lastThemeSamplingVisibility {
            lastThemeSamplingVisibility = presented
            onPresentationVisibilityChanged?()
        }
        let visible = window.map { $0.occlusionState.contains(.visible) } ?? false
        terminalView.setSurfaceVisible(visible)
        if visible {
            // Resume with a fresh frame so the viewport is current: the
            // pane may have been resized while detached, and output that
            // arrived while paused has not been drawn.
            terminalView.fitToSize()
            startTuiJumpHintPolling()
        } else {
            stopTuiJumpHintPolling()
        }
    }

    /// Re-derive surface visibility after an adoption swap. Swaps re-parent
    /// panes within the same window (warm host → swap container), so
    /// `viewDidMoveToWindow` — the usual visibility trigger — never fires,
    /// and the surface can stay marked occluded while displayed. Ghostty
    /// skips ALL rendering for occluded surfaces (updateFrame and draws), so
    /// a displayed-but-"occluded" pane parses output yet never repaints:
    /// an image-streaming app-mode session visibly froze between forced
    /// draws while its io thread happily consumed megabytes.
    func refreshSurfaceVisibility() {
        updateSurfaceVisibility()
    }

    /// Only the pane the user is actually looking at scans for the hint;
    /// detached/occluded panes pay nothing.
    private func startTuiJumpHintPolling() {
        guard tuiJumpHintTimer == nil else { return }
        let timer = Timer(timeInterval: 0.5, repeats: true) { [weak self] _ in
            MainActor.assumeIsolated {
                self?.scanForTuiJumpHint()
            }
        }
        timer.tolerance = 0.2
        RunLoop.main.add(timer, forMode: .common)
        tuiJumpHintTimer = timer
    }

    private func stopTuiJumpHintPolling() {
        tuiJumpHintTimer?.invalidate()
        tuiJumpHintTimer = nil
        tuiJumpRetriesRemaining = 0
        setTuiJumpHintActive(false)
    }

    private func scanForTuiJumpHint() {
        // No scrollback gate: Claude Code draws its virtual-scroll transcript
        // in the PRIMARY screen, so sessions with history would never show
        // the hint. Scanning unconditionally is safe because the button's
        // press does both — fakes ctrl+End (ignored by TUIs that aren't
        // scrolled up) and scrolls the local surface to the bottom — so even
        // a marker matched inside scrolled-back output yields a "jump to
        // bottom" outcome.
        guard let surface else {
            tuiJumpRetriesRemaining = 0
            setTuiJumpHintActive(false)
            return
        }
        let text = surface.readViewportText() ?? ""
        let active = Self.viewportHasTuiJumpHint(text)
        if active, tuiJumpRetriesRemaining > 0 {
            tuiJumpRetriesRemaining -= 1
            surface.sendKeyPress(keycode: Self.endKeycode, mods: GHOSTTY_MODS_CTRL)
            return
        }
        if !active {
            tuiJumpRetriesRemaining = 0
        }
        setTuiJumpHintActive(active)
    }

    private func setTuiJumpHintActive(_ active: Bool) {
        guard tuiJumpHintActive != active else { return }
        tuiJumpHintActive = active
        refreshScrollButtonVisibility()
    }

    private func refreshScrollButtonVisibility() {
        let visible = scrolledUpInScrollback || tuiJumpHintActive
        if scrollButtonModel.visible != visible {
            scrollButtonModel.visible = visible
        }
    }

    private func scrollButtonPressed() {
        if tuiJumpHintActive {
            // Hide optimistically and arm one retry: the poll re-sends the
            // key if the hint reappears, so one press settles at the bottom
            // even when the first ctrl+End lands mid-stream — while a
            // false-positive match leaks at most two "[1;5F" residues.
            tuiJumpRetriesRemaining = 1
            surface?.sendKeyPress(keycode: Self.endKeycode, mods: GHOSTTY_MODS_CTRL)
            setTuiJumpHintActive(false)
        }
        scrollToBottom()
    }

    // MARK: - Find bar behavior

    /// The pane the user is looking at: attached to the key window and not
    /// inside a hidden container (pre-warmed panes are mounted hidden).
    private var isDisplayedForFind: Bool {
        guard let window else { return false }
        return window.isKeyWindow && !isHiddenOrHasHiddenAncestor
    }

    func showFindBar() {
        let bar: TerminalFindBar
        if let existing = findBar {
            bar = existing
        } else {
            bar = TerminalFindBar()
            bar.onQueryChange = { [weak self] query in
                self?.applySearchQuery(query)
            }
            bar.onNext = { [weak self] in self?.findNext() }
            bar.onPrevious = { [weak self] in self?.findPrevious() }
            bar.onClose = { [weak self] in self?.hideFindBar() }
            bar.translatesAutoresizingMaskIntoConstraints = false
            addSubview(bar, positioned: .above, relativeTo: terminalView)
            NSLayoutConstraint.activate([
                bar.topAnchor.constraint(equalTo: topAnchor, constant: 10),
                bar.trailingAnchor.constraint(equalTo: trailingAnchor, constant: -12),
            ])
            findBar = bar
        }
        bar.isHidden = false
        findBarVisible = true
        // Reopening with a retained query re-arms the highlights that
        // end_search cleared.
        if !bar.query.isEmpty {
            applySearchQuery(bar.query)
        }
        bar.focusField()
    }

    func hideFindBar() {
        guard findBarVisible else { return }
        findBarVisible = false
        findBar?.isHidden = true
        searchTotal = nil
        searchSelected = nil
        _ = surface?.performBindingAction("end_search")
        focus()
    }

    func findNext() {
        guard findBarVisible else {
            showFindBar()
            return
        }
        _ = surface?.performBindingAction("navigate_search:next")
    }

    func findPrevious() {
        guard findBarVisible else {
            showFindBar()
            return
        }
        _ = surface?.performBindingAction("navigate_search:previous")
    }

    private func applySearchQuery(_ query: String) {
        // Fresh query, stale counts: clear until render-driven updates land.
        searchTotal = nil
        searchSelected = nil
        findBar?.updateCounts(total: nil, selected: nil)
        // Empty text cancels the search (keeps the bar up; end_search on
        // close tears the highlights down).
        _ = surface?.performBindingAction("search:\(query)")
    }

    // NOTE: an earlier override here trailing-debounced `fitToSize()` by
    // 80ms to coalesce per-frame PTY resizes. It never worked: super.layout()
    // applies the edge constraints, which drives AppTerminalView.setFrameSize
    // → fitToSize SYNCHRONOUSLY in the same pass — the debounced fit only
    // ever fired as a same-size no-op 80ms later. Resize smoothness is now
    // handled at the source (synchronous render + no-stretch gravity in the
    // wrapper; warm panes frozen during live resize in WarmPaneHostView).

    /// Route keyboard focus into the surface.
    func focus() {
        window?.makeFirstResponder(terminalView)
    }

    /// Draws a frame synchronously (no-op while detached or occluded).
    /// Called after adopting + focusing the pane on a session switch so the
    /// swap's own CATransaction presents current content with the focused
    /// cursor, instead of the stale pre-detach drawable for a frame or two
    /// followed by a hollow→filled cursor pop. `synchronousDraw` matters:
    /// output keeps parsing while a pane is occluded but draws are skipped,
    /// so on a same-size adoption the plain refresh()-only path would
    /// present the stale drawable first and snap to current content a tick
    /// later — the "row bounce" on fast session switches.
    func renderNow() {
        terminalView.renderImmediately(synchronousDraw: true)
    }

    /// Repairs terminal layout after a swap, once layout has settled (next
    /// runloop turn). When the pane's size or scale changed while it was
    /// detached, this re-runs the resize pipeline as if the user nudged the
    /// window edge a pixel and let go — a plain `fitToSize` after re-attach
    /// is a same-size no-op to ghostty, so it cannot fix that drift. The
    /// common no-drift switch skips the nudge entirely: forcing it on every
    /// switch cost two full resize passes plus PTY SIGWINCH churn that made
    /// the running TUI repaint, which read as switch lag next to Ghostty's
    /// instant tabs. Detached panes are skipped outright: a deferred refit
    /// can land after a rapid second switch already unhosted this pane, and
    /// with no window the scale falls back to `NSScreen.main` — a forced
    /// refit at that guessed scale would resize the hidden session's PTY to
    /// a wrong grid (adoption re-runs the fit anyway).
    func refitNow() {
        guard window != nil else { return }
        terminalView.refitIfDrifted()
    }

    /// Unconditional full resize pass — the 1px nudge — even when ghostty's
    /// own surface size never changed. That is the one repair `refitNow`
    /// deliberately skips, and it is the only way to make the attach client
    /// re-send its winsize downstream: a remote controller can resize the
    /// *shared hosted PTY* while this pane's surface stays put, so no local
    /// resize event ever fires and the desktop keeps rendering the diverged
    /// grid until something re-asserts it (previously: the user nudging the
    /// window edge by hand).
    func forceRefitNow() {
        guard window != nil else { return }
        terminalView.forceRefit()
    }

    // MARK: - Grid geometry (phone-resize letterbox)

    /// Grid of the most recently displayed full-bleed pane. New sessions
    /// open into the same terminal area, so this is the best estimate for
    /// their launch-time PTY size (`initial_cols`/`initial_rows`): the
    /// workload's first paint then matches the surface, instead of drawing
    /// at a guessed grid and depending on the attach client's corrective
    /// resize landing mid-startup (codex sometimes misses that SIGWINCH and
    /// keeps the wrong layout until the user nudges the window). nil until
    /// any pane has been displayed.
    static private(set) var lastDisplayedGrid: (cols: Int, rows: Int)?

    /// Set by the letterbox host while a phone-resize override constrains
    /// this pane: its grid then reflects the phone, not the terminal area,
    /// and must not feed `lastDisplayedGrid`.
    var isLetterboxed = false

    /// Latest surface grid + cell geometry in view points, and the view size
    /// it was measured against. Captured from the grid-resize delegate;
    /// backs `letterboxSize`.
    private var surfaceCellSize: CGSize?
    private var surfaceGridColumns = 0
    private var surfaceGridRows = 0
    private var surfaceBoundsAtSync = CGRect.zero

    /// Fired after the surface grid changes. The letterbox host uses it to
    /// re-fit once real cell metrics exist (a cold-mounted pane reports its
    /// first grid only after initial layout).
    var onSurfaceGridChanged: (() -> Void)?

    /// View size that renders exactly `cols`×`rows`: the target grid at the
    /// current cell size plus the surface chrome (window padding + sub-cell
    /// leftover) measured out of the last grid sync. Exact as long as the
    /// leftover stays under one cell, which the measurement guarantees.
    /// nil until the surface has reported a grid.
    func letterboxSize(cols: Int, rows: Int) -> CGSize? {
        guard let cell = surfaceCellSize,
              surfaceGridColumns > 0, surfaceGridRows > 0,
              surfaceBoundsAtSync.width > 1, surfaceBoundsAtSync.height > 1
        else { return nil }
        let chromeWidth = max(0, surfaceBoundsAtSync.width - CGFloat(surfaceGridColumns) * cell.width)
        let chromeHeight = max(0, surfaceBoundsAtSync.height - CGFloat(surfaceGridRows) * cell.height)
        return CGSize(
            width: CGFloat(cols) * cell.width + chromeWidth,
            height: CGFloat(rows) * cell.height + chromeHeight
        )
    }

    /// Rounds the pane's corners and draws a hairline bezel so a
    /// phone-letterboxed terminal reads as a phone screen. `masksToBounds`
    /// clips the Metal surface content to the rounded rect. Cleared (radius 0,
    /// no border) when the pane goes full-bleed again.
    func setPhoneScreenFraming(_ enabled: Bool) {
        wantsLayer = true
        guard let layer else { return }
        if enabled {
            layer.cornerRadius = 22
            layer.cornerCurve = .continuous
            layer.masksToBounds = true
            layer.borderWidth = 1
            layer.borderColor = NSColor.white.withAlphaComponent(0.14).cgColor
        } else {
            layer.cornerRadius = 0
            layer.masksToBounds = false
            layer.borderWidth = 0
            layer.borderColor = nil
        }
    }

    /// Injects text into the surface as if typed (used by the self-test).
    @discardableResult
    func sendText(_ text: String) -> Bool {
        surface?.sendText(text) ?? false
    }

    /// Snaps the viewport back to the live end of the screen, like the
    /// user pressing the scroll-to-bottom keybinding.
    func scrollToBottom() {
        surface?.performBindingAction("scroll_to_bottom")
    }

    /// Presses Return through the real AppKit key pipeline
    /// (keyDown -> ghostty_surface_key), exactly like a user keystroke.
    func pressReturn() {
        guard let event = NSEvent.keyEvent(
            with: .keyDown,
            location: .zero,
            modifierFlags: [],
            timestamp: ProcessInfo.processInfo.systemUptime,
            windowNumber: window?.windowNumber ?? 0,
            context: nil,
            characters: "\r",
            charactersIgnoringModifiers: "\r",
            isARepeat: false,
            keyCode: 36 // kVK_Return
        ) else {
            NSLog("[UnpeelNative] pressReturn: could not synthesize event")
            return
        }
        terminalView.keyDown(with: event)
    }

    /// SPIKE-ONLY diagnostics: dumps the terminal screen as plain text via
    /// `ghostty_surface_read_text`. The GhosttyTerminal wrapper keeps the raw
    /// `ghostty_surface_t` internal, so we pull it out with reflection. Do
    /// not ship this; ask upstream for a public accessor instead.
    func dumpScreenText() -> String? {
        guard let surface else { return nil }
        guard let raw = Mirror(reflecting: surface).children
            .first(where: { $0.label == "surface" })?
            .value as? ghostty_surface_t
        else {
            NSLog("[UnpeelNative] dumpScreenText: raw surface not reachable")
            return nil
        }

        var selection = ghostty_selection_s()
        selection.top_left = ghostty_point_s(
            tag: GHOSTTY_POINT_SCREEN, coord: GHOSTTY_POINT_COORD_TOP_LEFT, x: 0, y: 0
        )
        selection.bottom_right = ghostty_point_s(
            tag: GHOSTTY_POINT_SCREEN, coord: GHOSTTY_POINT_COORD_BOTTOM_RIGHT, x: 0, y: 0
        )
        selection.rectangle = false

        var text = ghostty_text_s()
        guard ghostty_surface_read_text(raw, selection, &text) else { return nil }
        defer { ghostty_surface_free_text(raw, &text) }
        guard let bytes = text.text else { return nil }
        return String(
            decoding: UnsafeBufferPointer(
                start: UnsafeRawPointer(bytes).assumingMemoryBound(to: UInt8.self),
                count: Int(text.text_len)
            ),
            as: UTF8.self
        )
    }

    // MARK: - File drag-and-drop

    // fileprivate: RemoteGhosttyTerminalPane (below) shares the exact same
    // drop pipeline, so scoped-workspace terminals accept the same drags.
    private static let imagePasteboardType = NSPasteboard.PasteboardType("public.image")
    private static let pngPasteboardType = NSPasteboard.PasteboardType("public.png")
    fileprivate static let dropPasteboardTypes: [NSPasteboard.PasteboardType] = [
        .fileURL,
        .URL,
        .tiff,
        pngPasteboardType,
        imagePasteboardType,
    ]

    /// Dropping files from Finder or image data from another app pastes
    /// attachable references into the terminal. Agent CLIs (e.g. Claude Code)
    /// detect single-quoted or backslash-escaped paths as attachable files, but
    /// not double-quoted ones. Multiple files are space-separated, no trailing
    /// newline so the user can keep typing.
    override func draggingEntered(_ sender: NSDraggingInfo) -> NSDragOperation {
        guard Self.canReadDropReferences(from: sender.draggingPasteboard) else {
            finishAppDropHover()
            return []
        }
        updateAppDropHover(sender)
        return .copy
    }

    override func draggingUpdated(_ sender: NSDraggingInfo) -> NSDragOperation {
        guard Self.canReadDropReferences(from: sender.draggingPasteboard) else {
            finishAppDropHover()
            return []
        }
        updateAppDropHover(sender)
        return .copy
    }

    override func draggingExited(_ sender: NSDraggingInfo?) {
        finishAppDropHover()
    }

    override func performDragOperation(_ sender: NSDraggingInfo) -> Bool {
        let references = Self.dropReferences(from: sender.draggingPasteboard)
        guard !references.isEmpty else { return false }
        let text = references.map {
            Self.quoteDropReference(Self.conciseDropReference(
                $0,
                projectRoot: projectRootDirectory
            ))
        }.joined(separator: " ")
        if let cell = appDropTargetCell(sender),
           let sessionDirectory,
           TerminalDropTargetMap.writeEvent(
               kind: .drop,
               row: cell.row,
               column: cell.column,
               text: text,
               references: references,
               to: sessionDirectory
           )
        {
            resetAppDropHover()
            return true
        }
        resetAppDropHover()
        return insertDroppedText(text)
    }

    private func appDropTargetCell(
        _ sender: NSDraggingInfo
    ) -> (row: Int, column: Int)? {
        guard let sessionDirectory,
              let cell = terminalView.viewportCell(atWindowPoint: sender.draggingLocation),
              let map = TerminalDropTargetMap.load(from: sessionDirectory),
              map.accepts(
                  row: cell.row,
                  column: cell.column,
                  nowMilliseconds: TerminalDropTargetMap.nowMilliseconds
              )
        else { return nil }
        return cell
    }

    private func updateAppDropHover(_ sender: NSDraggingInfo) {
        guard let cell = appDropTargetCell(sender), let sessionDirectory else {
            finishAppDropHover()
            return
        }
        appDropTargetActive = true
        let now = ProcessInfo.processInfo.systemUptime
        let changed = lastAppDropHoverCell?.row != cell.row
            || lastAppDropHoverCell?.column != cell.column
        guard changed || now - lastAppDropHoverAt >= Self.appDropHoverInterval else {
            return
        }
        if TerminalDropTargetMap.writeEvent(
            kind: .hover,
            row: cell.row,
            column: cell.column,
            to: sessionDirectory
        ) {
            lastAppDropHoverCell = cell
            lastAppDropHoverAt = now
        }
    }

    private func finishAppDropHover() {
        guard appDropTargetActive else { return }
        if let sessionDirectory {
            _ = TerminalDropTargetMap.writeEvent(kind: .leave, to: sessionDirectory)
        }
        resetAppDropHover()
    }

    private func resetAppDropHover() {
        appDropTargetActive = false
        lastAppDropHoverCell = nil
        lastAppDropHoverAt = 0
    }

    /// Decode the explicit OSC 8 link used to arm a local path drag. A remote
    /// `file://host/path` link is rejected: presenting it as a Controller file
    /// URL would silently change which machine the path names.
    nonisolated static func localDragPath(fromLink raw: String?) -> String? {
        guard let raw,
              let url = URL(string: raw),
              url.isFileURL
        else { return nil }
        if let host = url.host?.lowercased(), !host.isEmpty, host != "localhost" {
            return nil
        }
        let path = url.standardizedFileURL.path
        guard path.hasPrefix("/") else { return nil }
        return path
    }

    private func draggablePath(
        atScreenRow row: Int,
        column: Int,
        rowText _: String
    ) -> String? {
        if let sessionDirectory,
           let map = TerminalPathDragMap.load(from: sessionDirectory),
           let path = map.path(
               atScreenRow: row,
               column: column,
               nowMilliseconds: TerminalPathDragMap.nowMilliseconds
           ),
           FileManager.default.fileExists(atPath: path)
        {
            return path
        }

        // Standard OSC 8 file links remain a useful fallback for terminal
        // programs that do not publish the richer row map. Some Ghostty link
        // modes only surface hover meaning while a modifier is held, which is
        // why the exact row map above is the primary direct-drag path.
        guard let path = Self.localDragPath(fromLink: hoveredLink),
              FileManager.default.fileExists(atPath: path)
        else { return nil }
        return path
    }

    fileprivate static func canReadDropReferences(from pasteboard: NSPasteboard) -> Bool {
        if pasteboard.canReadObject(
            forClasses: [NSURL.self],
            options: [.urlReadingFileURLsOnly: true]
        ) {
            return true
        }
        if pasteboard.canReadObject(forClasses: [NSURL.self], options: nil) {
            return true
        }
        return pasteboard.canReadObject(forClasses: [NSImage.self], options: nil)
    }

    /// `home` picks whose `dropped-images` dir stabilizes volatile content —
    /// this instance's own for local terminals, the scoped workspace's for
    /// its panes (any local absolute path is readable machine-wide; the
    /// scope's home just keeps the artifact with its owner).
    fileprivate static func dropReferences(
        from pasteboard: NSPasteboard,
        home: URL = LaunchConfig.unpeelDir
    ) -> [String] {
        if let fileURLs = pasteboard.readObjects(
            forClasses: [NSURL.self],
            options: [.urlReadingFileURLsOnly: true]
        ) as? [URL], !fileURLs.isEmpty {
            return fileURLs.map { stableDropPath($0, home: home) }
        }

        if let urls = pasteboard.readObjects(
            forClasses: [NSURL.self],
            options: nil
        ) as? [URL] {
            let references = urls.map { url in
                url.isFileURL ? url.path : url.absoluteString
            }
            if !references.isEmpty { return references }
        }

        guard let images = pasteboard.readObjects(
            forClasses: [NSImage.self],
            options: nil
        ) as? [NSImage] else {
            return []
        }
        return images.compactMap { saveDroppedImage($0, home: home) }
    }

    /// macOS screenshot thumbnails (and other drags from a temporary
    /// location) point at volatile files under `.../TemporaryItems/
    /// NSIRD_screencaptureui…` that the OS deletes the moment the screenshot
    /// UI finalizes — so pasting their raw path yields a dead reference.
    /// Copy such files into the stable `dropped-images` dir (same place the
    /// image-data drop and the phone attach flow use) and reference the copy.
    /// Files already in a durable location are referenced in place.
    private static func stableDropPath(
        _ url: URL, home: URL = LaunchConfig.unpeelDir
    ) -> String {
        guard isVolatileLocation(url), let data = try? Data(contentsOf: url) else {
            return url.path
        }
        let dir = home.appendingPathComponent(
            "dropped-images",
            isDirectory: true
        )
        do {
            try FileManager.default.createDirectory(
                at: dir,
                withIntermediateDirectories: true
            )
            let ext = url.pathExtension.isEmpty ? "png" : url.pathExtension
            let timestamp = UInt64(Date().timeIntervalSince1970 * 1000)
            let filename = "drop-\(timestamp)-\(UUID().uuidString).\(ext)"
            let dest = dir.appendingPathComponent(filename)
            try data.write(to: dest, options: .atomic)
            return dest.path
        } catch {
            NSLog("[UnpeelNative] failed to stabilize dropped file: \(error)")
            return url.path
        }
    }

    private static func isVolatileLocation(_ url: URL) -> Bool {
        let path = url.path
        if path.contains("/TemporaryItems/") { return true }
        if path.localizedCaseInsensitiveContains("screencaptureui") { return true }
        if path.hasPrefix(NSTemporaryDirectory()) { return true }
        // Per-user sandbox temp: /var/folders/<…>/T/
        if path.contains("/var/folders/"), path.contains("/T/") { return true }
        return false
    }

    /// A dropped item destined for a TRUE remote Host: image content that
    /// must upload (Controller paths mean nothing there), or plain text
    /// (non-file links) pasted as-is.
    enum RemoteDropPayload {
        case upload(contentType: String, data: Data)
        case text(String)
    }

    /// Extract uploadable payloads synchronously (pasteboard data does not
    /// survive past the drop callback). Image files carry their own bytes;
    /// non-JPEG/PNG images are converted to PNG; non-image files are skipped
    /// — a remote Host has no way to receive them yet.
    fileprivate static func remoteDropPayloads(
        from pasteboard: NSPasteboard
    ) -> [RemoteDropPayload] {
        if let fileURLs = pasteboard.readObjects(
            forClasses: [NSURL.self],
            options: [.urlReadingFileURLsOnly: true]
        ) as? [URL], !fileURLs.isEmpty {
            return fileURLs.compactMap { url in
                switch url.pathExtension.lowercased() {
                case "png":
                    return (try? Data(contentsOf: url)).map {
                        .upload(contentType: "image/png", data: $0)
                    }
                case "jpg", "jpeg":
                    return (try? Data(contentsOf: url)).map {
                        .upload(contentType: "image/jpeg", data: $0)
                    }
                default:
                    // Other image formats convert; everything else is skipped.
                    guard let image = NSImage(contentsOf: url),
                          let data = imagePNGData(image)
                    else { return nil }
                    return .upload(contentType: "image/png", data: data)
                }
            }
        }
        if let urls = pasteboard.readObjects(
            forClasses: [NSURL.self],
            options: nil
        ) as? [URL] {
            let references = urls.compactMap { url in
                url.isFileURL ? nil : RemoteDropPayload.text(url.absoluteString)
            }
            if !references.isEmpty { return references }
        }
        guard let images = pasteboard.readObjects(
            forClasses: [NSImage.self],
            options: nil
        ) as? [NSImage] else {
            return []
        }
        return images.compactMap { image in
            imagePNGData(image).map { .upload(contentType: "image/png", data: $0) }
        }
    }

    private static func imagePNGData(_ image: NSImage) -> Data? {
        guard let tiff = image.tiffRepresentation,
              let rep = NSBitmapImageRep(data: tiff)
        else { return nil }
        return rep.representation(using: .png, properties: [:])
    }

    private static func saveDroppedImage(
        _ image: NSImage, home: URL = LaunchConfig.unpeelDir
    ) -> String? {
        guard let tiff = image.tiffRepresentation,
              let rep = NSBitmapImageRep(data: tiff),
              let data = rep.representation(using: .png, properties: [:])
        else { return nil }

        let dir = home.appendingPathComponent(
            "dropped-images",
            isDirectory: true
        )
        do {
            try FileManager.default.createDirectory(
                at: dir,
                withIntermediateDirectories: true
            )
            let timestamp = UInt64(Date().timeIntervalSince1970 * 1000)
            let filename = "drop-\(timestamp)-\(UUID().uuidString).png"
            let url = dir.appendingPathComponent(filename)
            try data.write(to: url, options: .atomic)
            return url.path
        } catch {
            NSLog("[UnpeelNative] failed to save dropped image: \(error)")
            return nil
        }
    }

    /// Inserts a file path into the prompt the same way a Finder drop does:
    /// quoted so agent CLIs detect it as an attachable reference, then typed
    /// (or pasted) into the focused surface. Used by the session gallery's
    /// "Add to prompt".
    @discardableResult
    func insertAttachablePath(_ path: String) -> Bool {
        insertDroppedText(Self.quoteDropReference(path))
    }

    private func insertDroppedText(_ text: String) -> Bool {
        focus()
        // Paste, don't type: agent TUIs only recognize an image path (and
        // collapse it to their "[Image #N]" attachment chip) when it arrives
        // as a bracketed paste. Ghostty's paste binding wraps in the paste
        // markers exactly when the app has enabled them, so this is also
        // safe for plain shells. sendText stays as the no-clipboard
        // fallback — it types the raw path.
        if pasteText(text) {
            return true
        }
        return sendText(text)
    }

    private func pasteText(_ text: String) -> Bool {
        guard let surface else { return false }
        return Self.clipboardPaste(into: surface, text: text) || sendText(text)
    }

    /// Paste through Ghostty's own binding so bracketed-paste markers follow
    /// the app's mode. Shared with the remote pane: HOST_MANAGED surfaces
    /// route the resulting bytes through their write closure to the Host.
    fileprivate static func clipboardPaste(
        into surface: TerminalSurface, text: String
    ) -> Bool {
        let pasteboard = NSPasteboard.general
        // NSPasteboardItem is not NSCopying — copy() throws. Snapshot the
        // clipboard by reading each item's data into a fresh item instead.
        let previousItems: [NSPasteboardItem] = (pasteboard.pasteboardItems ?? []).map { item in
            let snapshot = NSPasteboardItem()
            for type in item.types {
                if let data = item.data(forType: type) {
                    snapshot.setData(data, forType: type)
                }
            }
            return snapshot
        }

        pasteboard.clearContents()
        guard pasteboard.setString(text, forType: .string) else {
            restorePasteboardItems(previousItems)
            return false
        }
        let pasted = surface.performBindingAction("paste_from_clipboard")
        restorePasteboardItems(previousItems)
        return pasted
    }

    private static func restorePasteboardItems(_ items: [NSPasteboardItem]) {
        let pasteboard = NSPasteboard.general
        pasteboard.clearContents()
        if !items.isEmpty {
            pasteboard.writeObjects(items)
        }
    }

    /// Same safe-character set as the Svelte app's drop handler.
    nonisolated static func quoteDropReference(_ reference: String) -> String {
        guard reference.range(of: "[^\\w@%+=:,./-]", options: .regularExpression) != nil else {
            return reference
        }
        return "'" + reference.replacingOccurrences(of: "'", with: "'\\''") + "'"
    }

    /// Keep semantic drag data absolute, but make the text pasted into a
    /// local terminal human-sized: project-relative first, then `~/…`, then
    /// the original absolute path. Image paths deliberately stay absolute:
    /// Claude, Codex, and similar agents use that form to recognize a local
    /// image attachment and turn it into `[Image #…]`. Component-boundary
    /// checks prevent a root such as `/work/app` from shortening
    /// `/work/application/file`.
    nonisolated static func conciseDropReference(
        _ reference: String,
        projectRoot: String?,
        homeDirectory: String = NSHomeDirectory()
    ) -> String {
        guard (reference as NSString).isAbsolutePath else { return reference }
        let path = (reference as NSString).standardizingPath

        if isImageDropReference(path) {
            return path
        }

        if let projectRoot,
           let relative = pathRelativeToRoot(path, root: projectRoot) {
            return relative.isEmpty ? "." : relative
        }
        if let relative = pathRelativeToRoot(path, root: homeDirectory) {
            return relative.isEmpty ? "~" : "~/\(relative)"
        }
        return path
    }

    private nonisolated static func isImageDropReference(_ path: String) -> Bool {
        switch (path as NSString).pathExtension.lowercased() {
        case "png", "jpg", "jpeg", "gif", "webp", "heic", "heif", "tif", "tiff",
             "bmp", "avif", "svg":
            return true
        default:
            return false
        }
    }

    private nonisolated static func pathRelativeToRoot(
        _ path: String,
        root: String
    ) -> String? {
        let root = (root as NSString).standardizingPath
        guard !root.isEmpty else { return nil }
        if path == root { return "" }
        let prefix = root == "/" ? "/" : root + "/"
        guard path.hasPrefix(prefix) else { return nil }
        return String(path.dropFirst(prefix.count))
    }

    /// Enables the wrapper's stderr debug log (lifecycle, input, actions).
    static func enableDebugLogging() {
        TerminalDebugLog.enable(.standard)
    }
}

// MARK: - GhosttyTerminal delegates → plain delegate

extension GhosttyTerminalPane:
    TerminalSurfaceTitleDelegate,
    TerminalSurfaceGridResizeDelegate,
    TerminalSurfaceCloseDelegate,
    TerminalSurfaceBellDelegate,
    TerminalSurfacePwdDelegate,
    TerminalSurfaceOpenURLDelegate,
    TerminalSurfaceHoverLinkDelegate,
    TerminalSurfaceClickableFileDelegate,
    TerminalSurfaceScrollbarDelegate,
    TerminalSurfaceSearchDelegate,
    TerminalSurfaceLifecycleDelegate
{
    func terminalDidAttachSurface(_ surface: TerminalSurface) {
        self.surface = surface
        NSLog("[UnpeelNative] surface attached")
    }

    func terminalDidDetachSurface() {
        surface = nil
        hoveredLink = nil
        scrolledUpInScrollback = false
        tuiJumpHintActive = false
        lastScrollbarMetrics = nil
        scrollButtonModel.visible = false
        NSLog("[UnpeelNative] surface detached")
    }

    func terminalDidUpdateHoverLink(_ url: String?) {
        hoveredLink = url
    }

    func terminalDidUpdateScrollbar(_ metrics: TerminalScrollbarMetrics) {
        // In the alternate screen (full-screen TUIs) there is no
        // scrollback: total == viewport, so this path never fires "scrolled
        // up" — the TUI jump-hint scan covers that case instead.
        lastScrollbarMetrics = metrics
        scrolledUpInScrollback = !metrics.isAtBottom
        refreshScrollButtonVisibility()
    }

    // MARK: TerminalSurfaceSearchDelegate

    func terminalDidRequestStartSearch(needle _: String?) {
        // Keybinds are cleared, so today this only ever echoes our own
        // driving; keep it as the convergence point in case a future
        // binding or core path raises it.
        if !findBarVisible { showFindBar() }
    }

    func terminalDidRequestEndSearch() {
        // Core-initiated end: hide without re-sending end_search.
        guard findBarVisible else { return }
        findBarVisible = false
        findBar?.isHidden = true
        searchTotal = nil
        searchSelected = nil
    }

    func terminalDidUpdateSearchTotal(_ total: Int) {
        searchTotal = total
        findBar?.updateCounts(total: searchTotal, selected: searchSelected)
    }

    func terminalDidUpdateSearchSelected(_ selected: Int) {
        searchSelected = selected
        findBar?.updateCounts(total: searchTotal, selected: searchSelected)
    }

    func terminalDidChangeTitle(_ title: String) {
        paneDelegate?.terminalPane(self, didChangeTitle: title)
    }

    func terminalDidResize(_ size: TerminalGridMetrics) {
        let scale = max(window?.backingScaleFactor ?? 2, 1)
        surfaceGridColumns = Int(size.columns)
        surfaceGridRows = Int(size.rows)
        if size.cellWidthPixels > 0, size.cellHeightPixels > 0 {
            surfaceCellSize = CGSize(
                width: CGFloat(size.cellWidthPixels) / scale,
                height: CGFloat(size.cellHeightPixels) / scale
            )
        }
        surfaceBoundsAtSync = terminalView.bounds
        // Only a pane that is actually in the window reflects the terminal
        // area; pre-warmed/detached panes report layout-less default grids.
        if window != nil, !isLetterboxed, size.columns >= 20, size.rows >= 5 {
            Self.lastDisplayedGrid = (cols: Int(size.columns), rows: Int(size.rows))
        }
        // (No log here: this fires on every grid change during a drag, and
        // the wrapper already logs the same transition at .metrics level.)
        onSurfaceGridChanged?()
    }

    func terminalDidClose(processAlive: Bool) {
        paneDelegate?.terminalPane(self, didCloseProcessAlive: processAlive)
    }

    // Log-and-stub the rest for the spike.

    func terminalDidRingBell() {
        NSLog("[UnpeelNative] bell (stub)")
    }

    func terminalDidChangeWorkingDirectory(_ path: String) {
        // OSC 7 reports the shell's cwd; keep it so cmd-click resolves relative
        // paths against where the agent is actually working, not just the
        // spawn dir. Some agents/shells never emit it — the seed cwd remains.
        currentWorkingDirectory = path
    }

    /// Cmd-clicked a file path: resolve it against the session cwd and open it
    /// in the user's editor. Returns true only when it resolves to a real file
    /// (so a cmd-click on a URL or plain text falls through to normal handling).
    func terminalDidCommandClick(rowText: String, column: Int) -> Bool {
        guard let match = ClickablePath.match(inRow: rowText, column: column) else {
            return false
        }
        if let resolved = ClickablePath.absolutePath(
            match.path,
            workingDirectory: currentWorkingDirectory
        ), commandClickHandler?(match, resolved) == true {
            return true
        }
        guard let resolved = resolveClickedFile(match.path) else { return false }
        UnpeelStore.openFileInPreferredEditor(
            path: resolved,
            line: match.line,
            column: match.column
        )
        return true
    }

    /// Turns a clicked path token into an absolute path to an existing file, or
    /// nil. Relative paths resolve against the pane's cwd: seeded from the
    /// Session (`UnpeelStore.paneWorkingDirectory`), then replaced by the
    /// shell's OSC 7 reports.
    private func resolveClickedFile(_ raw: String) -> String? {
        ClickablePath.resolveFile(raw, workingDirectory: currentWorkingDirectory)
    }

    func terminalDidRequestOpenURL(_ url: String, kind _: TerminalOpenURLKind) {
        guard let parsed = Self.sanitizedURL(from: url) else {
            NSLog("[UnpeelNative] refusing malformed/unsupported url: \(url)")
            return
        }
        NSLog("[UnpeelNative] open url \(parsed.absoluteString)")
        if !NSWorkspace.shared.open(parsed) {
            NSLog("[UnpeelNative] NSWorkspace failed to open \(parsed.absoluteString)")
        }
    }

    /// Turn whatever the terminal handed us into something LaunchServices can
    /// actually open. Terminal-detected links routinely arrive wrapped across
    /// lines, padded with whitespace, or fenced in markdown punctuation like
    /// `(https://…)` / `<https://…>`; `URL(string:)` happily parses that junk
    /// into a URL that `NSWorkspace.open` then rejects with `-50` (paramErr).
    /// We strip the noise, re-encode if needed, and only allow safe schemes.
    nonisolated static func sanitizedURL(from raw: String) -> URL? {
        // Drop internal whitespace/newlines (a URL never legitimately contains
        // any — wrapped links pick these up from the terminal grid).
        var s = raw.components(separatedBy: .whitespacesAndNewlines).joined()
        guard !s.isEmpty else { return nil }

        // Peel matched wrapping brackets/quotes, then trailing punctuation that
        // commonly hugs an inline link (sentence periods, commas, etc.).
        let wrappers: [(Character, Character)] = [
            ("(", ")"), ("[", "]"), ("{", "}"), ("<", ">"),
            ("\"", "\""), ("'", "'"), ("`", "`"),
        ]
        for (open, close) in wrappers where s.first == open && s.last == close && s.count >= 2 {
            s = String(s.dropFirst().dropLast())
        }
        while let last = s.last, ".,;:!?\"')]}>".contains(last) {
            s = String(s.dropLast())
        }
        guard !s.isEmpty else { return nil }

        // Add a scheme for bare hosts so `www.example.com` / `example.com/x`
        // still open in the browser instead of being treated as a file path.
        if !s.contains("://"), !s.hasPrefix("mailto:"), !s.hasPrefix("tel:") {
            let host = s.split(separator: "/").first.map(String.init) ?? s
            if host.contains(".") {
                s = "https://" + s
            }
        }

        // Build the URL, percent-encoding leftover illegal characters if the
        // strict parse fails.
        let parsed = URL(string: s)
            ?? s.addingPercentEncoding(withAllowedCharacters: .urlQueryAllowed).flatMap(URL.init(string:))
        guard let parsed, let scheme = parsed.scheme?.lowercased() else { return nil }

        let allowed: Set<String> = ["http", "https", "mailto", "tel", "ftp", "ftps"]
        guard allowed.contains(scheme) else { return nil }
        return parsed
    }
}

// MARK: - Remote Host terminal surface

/// Ghostty-free viewport value delivered to the remote transport whenever
/// the Controller's pane changes size. The transport decides how and when to
/// send it to the Host; this bridge never reaches into local session state.
struct RemoteTerminalViewport: Equatable, Sendable {
    let columns: Int
    let rows: Int
    let widthPixels: Int
    let heightPixels: Int
    let cellWidthPixels: Int
    let cellHeightPixels: Int

    fileprivate init(_ viewport: InMemoryTerminalViewport) {
        columns = Int(viewport.columns)
        rows = Int(viewport.rows)
        widthPixels = Int(viewport.widthPixels)
        heightPixels = Int(viewport.heightPixels)
        cellWidthPixels = Int(viewport.cellWidthPixels)
        cellHeightPixels = Int(viewport.cellHeightPixels)
    }
}

typealias RemoteTerminalInputHandler = @MainActor @Sendable (Data) -> Void
typealias RemoteTerminalResizeHandler = @MainActor @Sendable (RemoteTerminalViewport) -> Void

/// Token captured when a terminal callback is enqueued. Rebinding or clearing
/// a pane advances the token, so already-queued input/resize is discarded
/// instead of crossing into the replacement transport.
struct RemoteTerminalCallbackEpoch: Equatable, Sendable {
    private(set) var revision: UInt64 = 0

    mutating func advance() {
        revision &+= 1
    }

    func accepts(_ queuedEpoch: RemoteTerminalCallbackEpoch) -> Bool {
        self == queuedEpoch
    }
}

/// Mutable callback indirection for a retained pane. A reconnect may replace
/// the transport while the Ghostty surface (and its last rendered frame)
/// stays alive, so the in-memory session must not permanently capture the
/// connection that happened to create it.
private final class RemoteTerminalCallbackRelay: @unchecked Sendable {
    private let lock = NSLock()
    private var inputHandler: RemoteTerminalInputHandler
    private var resizeHandler: RemoteTerminalResizeHandler
    private var epoch = RemoteTerminalCallbackEpoch()
    /// False while the owning pane is detached from a window or has
    /// presentation disabled. Ghostty core emits garbage geometry in that
    /// state (default ~50x17 grids from the unhosted layer), and one such
    /// late emission after a real fit was what kept shrinking scoped
    /// workspaces' Host PTYs. Resizes while not live are DROPPED — a live
    /// re-present always refits from real bounds.
    private var resizeLive = true

    func setResizeLive(_ live: Bool) {
        lock.lock()
        resizeLive = live
        lock.unlock()
    }

    private func isResizeLive() -> Bool {
        lock.lock()
        defer { lock.unlock() }
        return resizeLive
    }

    init(
        input: @escaping RemoteTerminalInputHandler,
        resize: @escaping RemoteTerminalResizeHandler
    ) {
        inputHandler = input
        resizeHandler = resize
    }

    func update(
        input: @escaping RemoteTerminalInputHandler,
        resize: @escaping RemoteTerminalResizeHandler
    ) {
        lock.lock()
        epoch.advance()
        inputHandler = input
        resizeHandler = resize
        lock.unlock()
    }

    func sendInput(_ data: Data) {
        let queuedEpoch = currentEpoch()
        deliverOnMain { relay in
            relay.deliverInput(data, queuedEpoch: queuedEpoch)
        }
    }

    func sendResize(_ viewport: InMemoryTerminalViewport) {
        guard isResizeLive() else { return }
        let queuedEpoch = currentEpoch()
        let plainViewport = RemoteTerminalViewport(viewport)
        deliverOnMain { relay in
            relay.deliverResize(plainViewport, queuedEpoch: queuedEpoch)
        }
    }

    /// Break transport/runtime captures as soon as a pane is evicted. The
    /// Ghostty teardown itself is deferred by one main-queue turn, so relying
    /// on deinit alone would leave a brief window where input could still hit
    /// a retired connection.
    func clear() {
        lock.lock()
        epoch.advance()
        inputHandler = { _ in }
        resizeHandler = { _ in }
        lock.unlock()
    }

    private func currentEpoch() -> RemoteTerminalCallbackEpoch {
        lock.lock()
        let value = epoch
        lock.unlock()
        return value
    }

    private func deliverOnMain(
        _ body: @escaping @MainActor @Sendable (RemoteTerminalCallbackRelay) -> Void
    ) {
        if Thread.isMainThread {
            MainActor.assumeIsolated {
                body(self)
            }
        } else {
            DispatchQueue.main.async { [weak self] in
                guard let self else { return }
                body(self)
            }
        }
    }

    @MainActor
    private func deliverInput(
        _ data: Data,
        queuedEpoch: RemoteTerminalCallbackEpoch
    ) {
        lock.lock()
        guard epoch.accepts(queuedEpoch) else {
            lock.unlock()
            return
        }
        let handler = inputHandler
        lock.unlock()
        handler(data)
    }

    @MainActor
    private func deliverResize(
        _ viewport: RemoteTerminalViewport,
        queuedEpoch: RemoteTerminalCallbackEpoch
    ) {
        lock.lock()
        guard epoch.accepts(queuedEpoch) else {
            lock.unlock()
            return
        }
        let handler = resizeHandler
        lock.unlock()
        handler(viewport)
    }
}

/// Bytes that are injected only into the Controller's local VT parser. They
/// are never routed through `InMemoryTerminalSession.sendInput`, so resetting
/// a retained frame cannot write escape sequences to the Host PTY.
struct RemoteTerminalLocalFeed: Equatable, Sendable {
    let bytes: Data

    /// CAN aborts an unterminated OSC/DCS before RIS. A retention rebase may
    /// deliberately cut such a pathological control string to keep an
    /// always-on journal bounded, so ESC c alone could be swallowed.
    private static let reset = Data([0x18, 0x1B, 0x63])
    private static let beginSynchronizedOutput = Data("\u{1B}[?2026h".utf8)
    private static let clearDisplayAndScrollback = Data(
        "\u{1B}[3J\u{1B}[2J\u{1B}[H".utf8
    )
    private static let endSynchronizedOutput = Data("\u{1B}[?2026l".utf8)

    /// Standalone reset: RIS clears terminal modes; CSI 3J/2J/H clears the
    /// retained screen, scrollback, and cursor. The synchronized-output pair
    /// prevents an intermediate blank frame from presenting.
    static let resetRetainedState = RemoteTerminalLocalFeed(
        bytes: reset
            + beginSynchronizedOutput
            + clearDisplayAndScrollback
            + endSynchronizedOutput
    )

    /// Atomic reset + replacement output. RIS must precede DEC 2026 because
    /// RIS itself resets synchronized-output mode.
    static func resettingBeforeFeeding(_ payload: Data) -> RemoteTerminalLocalFeed {
        RemoteTerminalLocalFeed(
            bytes: reset
                + beginSynchronizedOutput
                + clearDisplayAndScrollback
                + payload
                + endSynchronizedOutput
        )
    }
}

/// A retained, GPU-rendered terminal whose process and byte stream are owned
/// by a remote Host. This is intentionally a different type from
/// ``GhosttyTerminalPane``:
///
/// - its backend is `InMemoryTerminalSession`, never Ghostty's EXEC backend;
/// - it has no command or working directory and cannot launch `unpeel-attach`;
/// - host output enters only through ``receiveHostBytes(_:)``;
/// - keyboard/mouse input and viewport changes leave through plain-Swift
///   callbacks; and
/// - it does not implement URL/PWD delegates; file clicks are resolved from
///   the Host-reported cwd and routed back to that Host, never opened against
///   the Controller's filesystem.
///
/// Removing this view from a hierarchy pauses rendering but deliberately
/// keeps both the surface and its last frame alive. Reattaching the same pane
/// therefore paints immediately and preserves scrollback and VT state.
@MainActor
final class RemoteGhosttyTerminalPane: NSView {
    private let terminalView: TerminalView
    private let controller: TerminalController
    private let memorySession: InMemoryTerminalSession
    private let callbackRelay: RemoteTerminalCallbackRelay
    private var surface: TerminalSurface?
    private var paneStyle: TerminalPaneStyle
    private var occlusionObserver: NSObjectProtocol?
    private var presentationEnabled = true
    private var needsRefitOnNextPresentation = true
    private var currentWorkingDirectory: String?
    private var commandClickHandler: ((ClickablePath.Match, String) -> Bool)?

    init(
        style: TerminalPaneStyle = .resolved(),
        onInput: @escaping RemoteTerminalInputHandler,
        onResize: @escaping RemoteTerminalResizeHandler
    ) {
        paneStyle = style

        let relay = RemoteTerminalCallbackRelay(input: onInput, resize: onResize)
        callbackRelay = relay
        memorySession = InMemoryTerminalSession(
            write: { data in relay.sendInput(data) },
            resize: { viewport in relay.sendResize(viewport) }
        )

        // There is deliberately no `command` config. The surface below uses
        // HOST_MANAGED IO, so Ghostty parses/renders bytes but never spawns a
        // local process for a remote session.
        controller = TerminalController(theme: GhosttyTerminalPane.terminalTheme(for: style)) {
            builder in
            GhosttyTerminalPane.applySurfaceKeybinds(&builder)
            builder.withCustom("window-padding-x", "\(style.windowPaddingX)")
            builder.withCustom("window-padding-y", "\(style.windowPaddingY)")
            builder.withCustom(
                "window-padding-balance",
                style.windowPaddingBalanced ? "true" : "false"
            )
            builder.withCustom("window-padding-color", "extend")
            builder.withCustom(
                "mouse-scroll-multiplier",
                "precision:1,discrete:\(style.mouseScrollMultiplier)"
            )
            builder.withCursorStyle(.block)
            builder.withCursorStyleBlink(true)
            builder.withFontSize(style.fontSize)
            if let family = style.fontFamily {
                builder.withFontFamily(family)
            }
            builder.withBackgroundOpacity(style.backgroundOpacity)
        }

        if let issue = controller.lastConfigurationIssue {
            NSLog("[UnpeelNative] remote ghostty configuration issue: %@", issue)
        }

        terminalView = TerminalView(frame: .zero)
        terminalView.gridOrigin = CGPoint(
            x: CGFloat(style.windowPaddingX),
            y: CGFloat(style.windowPaddingY)
        )

        super.init(frame: .zero)

        applyFrameLayerBackground(style)
        terminalView.configuration = TerminalSurfaceOptions(
            backend: .inMemory(memorySession),
            workingDirectory: nil,
            context: .window
        )
        terminalView.delegate = self
        terminalView.controller = controller
        terminalView.translatesAutoresizingMaskIntoConstraints = false
        addSubview(terminalView)
        NSLayoutConstraint.activate([
            terminalView.topAnchor.constraint(equalTo: topAnchor),
            terminalView.leadingAnchor.constraint(equalTo: leadingAnchor),
            terminalView.trailingAnchor.constraint(equalTo: trailingAnchor),
            terminalView.bottomAnchor.constraint(equalTo: bottomAnchor),
        ])
        registerForDraggedTypes(GhosttyTerminalPane.dropPasteboardTypes)
    }

    func configureFileOpening(
        workingDirectory: String?,
        handler: ((ClickablePath.Match, String) -> Bool)?
    ) {
        currentWorkingDirectory = workingDirectory
        commandClickHandler = handler
    }

    // MARK: File drag-and-drop (scoped-workspace terminals)

    /// Enabled for LOCAL-MACHINE scopes: a scoped workspace's session runs on
    /// this Mac, so the same quoted local paths the local surface pastes are
    /// readable by it.
    var fileDropsEnabled = false
    /// Home whose `dropped-images` dir stabilizes volatile drops — the
    /// scoped workspace's own, so the artifact lives with its owner.
    var dropStabilizeHome: URL = LaunchConfig.unpeelDir
    /// TRUE remote Hosts: image content uploads through `artifact.upload`
    /// (the phone attach flow's operation) and the returned HOST path is
    /// pasted instead. nil when the Host does not advertise the capability.
    var remoteUploader: ((_ contentType: String, _ bytes: Data) async throws -> String)?

    override func draggingEntered(_ sender: NSDraggingInfo) -> NSDragOperation {
        (fileDropsEnabled || remoteUploader != nil)
            && GhosttyTerminalPane.canReadDropReferences(
                from: sender.draggingPasteboard
            ) ? .copy : []
    }

    override func draggingUpdated(_ sender: NSDraggingInfo) -> NSDragOperation {
        draggingEntered(sender)
    }

    override func performDragOperation(_ sender: NSDraggingInfo) -> Bool {
        if fileDropsEnabled {
            let references = GhosttyTerminalPane.dropReferences(
                from: sender.draggingPasteboard,
                home: dropStabilizeHome
            )
            guard !references.isEmpty else { return false }
            pasteDropReferences(references)
            return true
        }
        guard let remoteUploader else { return false }
        // Pasteboard content must be read before this callback returns; the
        // uploads themselves ride an ordinary async verb per payload.
        let payloads = GhosttyTerminalPane.remoteDropPayloads(
            from: sender.draggingPasteboard
        )
        guard !payloads.isEmpty else { return false }
        Task { @MainActor [weak self] in
            var references: [String] = []
            for payload in payloads {
                switch payload {
                case .text(let reference):
                    references.append(reference)
                case .upload(let contentType, let data):
                    do {
                        references.append(
                            try await remoteUploader(contentType, data)
                        )
                    } catch {
                        NSLog(
                            "[UnpeelNative] remote drop upload failed: %@",
                            error.localizedDescription
                        )
                    }
                }
            }
            guard !references.isEmpty else { return }
            self?.pasteDropReferences(references)
        }
        return true
    }

    /// Same paste-don't-type rule as the local surface: Ghostty wraps the
    /// bracketed-paste markers per the app's mode and the HOST_MANAGED
    /// surface routes the bytes through the input relay to the Host.
    private func pasteDropReferences(_ references: [String]) {
        let text = references
            .map(GhosttyTerminalPane.quoteDropReference)
            .joined(separator: " ")
        focus()
        if let surface, GhosttyTerminalPane.clipboardPaste(into: surface, text: text) {
            return
        }
        callbackRelay.sendInput(Data(text.utf8))
    }

    @available(*, unavailable)
    required init?(coder _: NSCoder) { fatalError("not supported") }

    deinit {
        MainActor.assumeIsolated {
            callbackRelay.clear()
            if let occlusionObserver {
                NotificationCenter.default.removeObserver(occlusionObserver)
            }
        }
    }

    /// Rebind a retained pane after transport reconnect without losing its
    /// terminal state. No bytes can escape to the superseded connection once
    /// this returns; the relay swaps both callbacks under one lock.
    func updateCallbacks(
        onInput: @escaping RemoteTerminalInputHandler,
        onResize: @escaping RemoteTerminalResizeHandler
    ) {
        callbackRelay.update(input: onInput, resize: onResize)
    }

    fileprivate func clearCallbacks() {
        callbackRelay.clear()
    }

    /// A remote output cursor may advance only after this pane accepts the
    /// corresponding bytes. Detached panes deliberately report not-ready:
    /// `InMemoryTerminalSession` can buffer pre-attach bytes, but doing so
    /// would make acceptance invisible to the runtime and could commit a Host
    /// cursor before any surface actually parsed the page.
    var isReadyForHostBytes: Bool {
        surface != nil
    }

    /// Feed an exact output page/chunk from the selected Host into Ghostty's
    /// VT engine. Returns true only after an attached surface synchronously
    /// accepts the whole feed. A detached pane refuses without invoking
    /// `memorySession.receive`, including when `resetBeforeFeed` is set, so
    /// callers must retain/retry the same uncommitted output page.
    @discardableResult
    func receiveHostBytes(_ data: Data, resetBeforeFeed: Bool = false) -> Bool {
        guard isReadyForHostBytes else { return false }
        let localFeed = resetBeforeFeed
            ? RemoteTerminalLocalFeed.resettingBeforeFeeding(data).bytes
            : data
        memorySession.receive(localFeed)
        return true
    }

    @discardableResult
    func receiveHostText(_ text: String) -> Bool {
        receiveHostBytes(Data(text.utf8))
    }

    /// Reset only the retained Controller-side VT state. Prefer the atomic
    /// `receiveHostBytes(_:resetBeforeFeed:)` path when replacement output is
    /// already available, so no blank reset frame can present between calls.
    @discardableResult
    func resetRetainedVTState() -> Bool {
        guard isReadyForHostBytes else { return false }
        memorySession.receive(RemoteTerminalLocalFeed.resetRetainedState.bytes)
        return true
    }

    /// Signal a Host-owned process exit to Ghostty. This never tears down or
    /// launches a Controller-side process.
    func finishHostProcess(exitCode: UInt32, runtimeMilliseconds: UInt64) {
        memorySession.finish(
            exitCode: exitCode,
            runtimeMilliseconds: runtimeMilliseconds
        )
    }

    /// Lets an owner keep a pane mounted during a transition without paying
    /// for hidden rendering. Host bytes and VT state continue to be retained.
    func setPresentationEnabled(_ enabled: Bool) {
        guard presentationEnabled != enabled else { return }
        presentationEnabled = enabled
        if !enabled {
            needsRefitOnNextPresentation = true
        }
        callbackRelay.setResizeLive(enabled && window != nil)
        updateSurfaceVisibility()
    }

    func applyPaneStyle(_ style: TerminalPaneStyle) {
        let opacityChanged = paneStyle.backgroundOpacity != style.backgroundOpacity
        guard !Self.hasSameTheme(paneStyle, style) || opacityChanged else { return }
        paneStyle = style
        _ = controller.setTheme(GhosttyTerminalPane.terminalTheme(for: style))
        if opacityChanged {
            _ = controller.setTerminalConfiguration(
                TerminalConfiguration {
                    $0.withBackgroundOpacity(style.backgroundOpacity)
                }
            )
        }
        applyFrameLayerBackground(style)
    }

    /// Live style updates intentionally cover colors only. Font and padding
    /// are surface geometry established at construction; callers that change
    /// those should evict/recreate the cache entry instead of reflowing a
    /// retained remote screen during a session switch.
    private static func hasSameTheme(
        _ lhs: TerminalPaneStyle,
        _ rhs: TerminalPaneStyle
    ) -> Bool {
        func same(
            _ lhs: TerminalPaneStyle.Variant,
            _ rhs: TerminalPaneStyle.Variant
        ) -> Bool {
            lhs.background == rhs.background
                && lhs.foreground == rhs.foreground
                && lhs.selectionBackground == rhs.selectionBackground
                && lhs.cursorColor == rhs.cursorColor
                && lhs.palette == rhs.palette
        }
        return same(lhs.light, rhs.light) && same(lhs.dark, rhs.dark)
    }

    override func viewDidChangeEffectiveAppearance() {
        super.viewDidChangeEffectiveAppearance()
        applyFrameLayerBackground(paneStyle)
    }

    override func viewDidMoveToWindow() {
        super.viewDidMoveToWindow()
        if let occlusionObserver {
            NotificationCenter.default.removeObserver(occlusionObserver)
            self.occlusionObserver = nil
        }
        if let window {
            occlusionObserver = NotificationCenter.default.addObserver(
                forName: NSWindow.didChangeOcclusionStateNotification,
                object: window,
                queue: .main
            ) { [weak self] _ in
                MainActor.assumeIsolated {
                    self?.updateSurfaceVisibility()
                }
            }
        } else {
            needsRefitOnNextPresentation = true
        }
        callbackRelay.setResizeLive(presentationEnabled && window != nil)
        updateSurfaceVisibility()
    }

    private func updateSurfaceVisibility() {
        let visible = presentationEnabled
            && (window?.occlusionState.contains(.visible) ?? false)
        terminalView.setSurfaceVisible(visible)
        guard visible else { return }

        terminalView.fitToSize()
        if needsRefitOnNextPresentation {
            needsRefitOnNextPresentation = false
            terminalView.forceRefit()
        }
        terminalView.renderImmediately()
    }

    private func applyFrameLayerBackground(_ style: TerminalPaneStyle) {
        wantsLayer = true
        // Same rule as the local pane: with a translucent terminal the
        // surface's background-opacity is the only canvas paint.
        if style.backgroundOpacity < 1 {
            layer?.backgroundColor = NSColor.clear.cgColor
            return
        }
        let appearance = window?.effectiveAppearance ?? effectiveAppearance
        let isDark = appearance.bestMatch(from: [.aqua, .darkAqua]) == .darkAqua
        let hex = isDark ? style.dark.background : style.light.background
        layer?.backgroundColor = (
            GhosttyTerminalPane.nsColor(fromHexString: hex)
                ?? Theme.terminalBackgroundNSColor
        ).cgColor
    }

    func focus() {
        window?.makeFirstResponder(terminalView)
    }

    func renderNow() {
        terminalView.renderImmediately(synchronousDraw: true)
    }

    func refitNow() {
        guard window != nil else { return }
        terminalView.refitIfDrifted()
        terminalView.renderImmediately()
    }

    func scrollToBottom() {
        surface?.performBindingAction("scroll_to_bottom")
    }

    func readViewportText() -> String? {
        memorySession.readViewportText()
    }

    /// The grid the surface last reported (cols/rows), or nil before the
    /// first resize. Presentation replays re-derive the fit from THIS —
    /// never from a stored viewport that may predate a scale/attach fix.
    func currentGrid() -> (columns: Int, rows: Int)? {
        guard let viewport = memorySession.lastReportedViewport(),
              viewport.columns > 0, viewport.rows > 0
        else { return nil }
        return (Int(viewport.columns), Int(viewport.rows))
    }
}

extension RemoteGhosttyTerminalPane:
    TerminalSurfaceLifecycleDelegate,
    TerminalSurfaceClickableFileDelegate
{
    func terminalDidAttachSurface(_ surface: TerminalSurface) {
        self.surface = surface
        // The surface is created while the view is still at .zero, finishes
        // attaching AFTER the first real fit has run, and then reports its
        // creation-time default grid (~50x17). That stale emission arrived
        // last, so it was the size the Host resized the remote PTY to — the
        // scoped-workspace "terminal stuck tiny until I resize the window"
        // bug. One post-attach refit makes the view-true grid the final
        // word.
        DispatchQueue.main.async { [weak self] in
            guard let self, self.window != nil else { return }
            self.terminalView.forceRefit()
            self.terminalView.renderImmediately()
        }
    }

    func terminalDidDetachSurface() {
        surface = nil
    }

    /// Remote paths are meaningful only on the Host. Returning false keeps
    /// ordinary selection behavior and, crucially, performs no Controller
    /// filesystem lookup or editor launch.
    func terminalDidCommandClick(rowText: String, column: Int) -> Bool {
        guard let match = ClickablePath.match(inRow: rowText, column: column),
              let path = ClickablePath.absolutePath(
                match.path,
                workingDirectory: currentWorkingDirectory
              )
        else { return false }
        return commandClickHandler?(match, path) == true
    }
}

/// Stable identity for a retained remote pane. Session ids are Host-local,
/// so the Host id must be part of every cache lookup.
struct RemoteTerminalPaneKey: Hashable, Sendable {
    let hostID: String
    let sessionID: String
}

/// Pure LRU bookkeeping shared by the real pane cache and focused tests.
/// Protected entries (normally the selected pane) may take the set above the
/// nominal limit, but are never evicted out from under the visible surface.
struct RemoteTerminalPaneRetention {
    let limit: Int
    private(set) var mostRecent: [RemoteTerminalPaneKey] = []

    init(limit: Int = 8) {
        self.limit = max(1, limit)
    }

    mutating func noteUsed(_ key: RemoteTerminalPaneKey) {
        mostRecent.removeAll { $0 == key }
        mostRecent.append(key)
    }

    mutating func remove(_ key: RemoteTerminalPaneKey) {
        mostRecent.removeAll { $0 == key }
    }

    mutating func retained(
        from available: Set<RemoteTerminalPaneKey>,
        protecting protected: Set<RemoteTerminalPaneKey> = []
    ) -> Set<RemoteTerminalPaneKey> {
        mostRecent.removeAll { !available.contains($0) }
        var keep = protected.intersection(available)
        for key in mostRecent.reversed() where keep.count < limit {
            keep.insert(key)
        }
        return keep
    }
}

/// Remote-only pane retention. It never consults the Local `SurfaceCache`,
/// local `SessionEntry` values, launch commands, or filesystem paths.
@MainActor
final class RemoteGhosttyPaneCache {
    static let retainedPaneLimit = 8

    private var panes: [RemoteTerminalPaneKey: RemoteGhosttyTerminalPane] = [:]
    private var retention: RemoteTerminalPaneRetention

    init(retainedPaneLimit: Int = RemoteGhosttyPaneCache.retainedPaneLimit) {
        retention = RemoteTerminalPaneRetention(limit: retainedPaneLimit)
    }

    func pane(
        for key: RemoteTerminalPaneKey,
        style: TerminalPaneStyle = .resolved(),
        onInput: @escaping RemoteTerminalInputHandler,
        onResize: @escaping RemoteTerminalResizeHandler,
        workingDirectory: String? = nil,
        onCommandClick: ((ClickablePath.Match, String) -> Bool)? = nil
    ) -> RemoteGhosttyTerminalPane {
        if let existing = panes[key] {
            existing.updateCallbacks(onInput: onInput, onResize: onResize)
            existing.applyPaneStyle(style)
            retention.noteUsed(key)
            existing.configureFileOpening(
                workingDirectory: workingDirectory,
                handler: onCommandClick
            )
            return existing
        }

        let pane = RemoteGhosttyTerminalPane(
            style: style,
            onInput: onInput,
            onResize: onResize
        )
        pane.configureFileOpening(
            workingDirectory: workingDirectory,
            handler: onCommandClick
        )
        panes[key] = pane
        retention.noteUsed(key)
        return pane
    }

    func existingPane(for key: RemoteTerminalPaneKey) -> RemoteGhosttyTerminalPane? {
        panes[key]
    }

    func noteShown(_ key: RemoteTerminalPaneKey) {
        guard panes[key] != nil else { return }
        retention.noteUsed(key)
    }

    func prune(
        keeping liveKeys: Set<RemoteTerminalPaneKey>,
        selectedKey: RemoteTerminalPaneKey?,
        protectedKeys: Set<RemoteTerminalPaneKey> = []
    ) {
        let protected = protectedKeys.union(selectedKey.map { Set([$0]) } ?? [])
        let keep = retention.retained(
            from: Set(panes.keys).intersection(liveKeys),
            protecting: protected
        )
        for key in Array(panes.keys) where !keep.contains(key) {
            drop(key)
        }
    }

    func removeHost(_ hostID: String) {
        for key in Array(panes.keys) where key.hostID == hostID {
            drop(key)
        }
    }

    func removeAll() {
        for key in Array(panes.keys) {
            drop(key)
        }
    }

    private func drop(_ key: RemoteTerminalPaneKey) {
        guard let pane = panes.removeValue(forKey: key) else { return }
        retention.remove(key)
        pane.clearCallbacks()
        pane.setPresentationEnabled(false)

        // Surface teardown can release Metal resources. Move that work out
        // of the SwiftUI/layout pass that decided to prune, just like the
        // Local cache does, while preserving this cache as a separate owner.
        DispatchQueue.main.async {
            pane.removeFromSuperview()
        }
    }
}
