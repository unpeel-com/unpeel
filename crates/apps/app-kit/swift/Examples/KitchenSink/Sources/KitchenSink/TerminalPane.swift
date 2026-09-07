import AppKit
import GhosttyTerminal
import SwiftUI

struct TerminalLaunch: Sendable {
    let executable: String
    let currentDirectory: String
    let environment: [String: String]
}

/// libghostty owns the PTY, VT state, selection, clipboard integration, and
/// Metal rendering. The mini-host only supplies the command and environment.
final class KitchenSinkTerminalView: TerminalView {
    override func acceptsFirstMouse(for _: NSEvent?) -> Bool {
        true
    }

    override func mouseDown(with event: NSEvent) {
        window?.makeFirstResponder(self)
        super.mouseDown(with: event)
    }
}

/// Stable wrapper retained across process restarts and presentation switches.
final class KitchenSinkTerminalHostView: NSView {
    private(set) weak var terminalView: KitchenSinkTerminalView?
    var wantsInitialFocus = false

    func install(_ terminalView: KitchenSinkTerminalView?) {
        self.terminalView?.removeFromSuperview()
        self.terminalView = terminalView
        guard let terminalView else { return }
        terminalView.translatesAutoresizingMaskIntoConstraints = false
        addSubview(terminalView)
        NSLayoutConstraint.activate([
            terminalView.topAnchor.constraint(equalTo: topAnchor),
            terminalView.leadingAnchor.constraint(equalTo: leadingAnchor),
            terminalView.trailingAnchor.constraint(equalTo: trailingAnchor),
            terminalView.bottomAnchor.constraint(equalTo: bottomAnchor),
        ])
        requestFocusIfNeeded()
    }

    override func viewDidMoveToWindow() {
        super.viewDidMoveToWindow()
        requestFocusIfNeeded()
    }

    func requestFocus() {
        wantsInitialFocus = true
        requestFocusIfNeeded()
    }

    private func requestFocusIfNeeded() {
        guard wantsInitialFocus, let terminalView, window != nil else { return }
        wantsInitialFocus = false
        DispatchQueue.main.async { [weak terminalView] in
            terminalView?.acquireProgrammaticFocus()
        }
    }
}

@MainActor
final class TerminalEngineController: NSObject {
    let view = KitchenSinkTerminalHostView(frame: .zero)
    var onTermination: ((Int32?) -> Void)?
    private(set) var isRunning = false

    private let parkingWindow: NSWindow
    private let parkingView = NSView(frame: CGRect(x: 0, y: 0, width: 900, height: 600))
    private var terminalView: KitchenSinkTerminalView?
    private var terminalController: TerminalController?
    private var isDisplayed = false

    override init() {
        parkingWindow = NSWindow(
            contentRect: CGRect(x: 0, y: 0, width: 900, height: 600),
            styleMask: .borderless,
            backing: .buffered,
            defer: false
        )
        super.init()
        parkingWindow.contentView = parkingView
        park()
    }

    func start(_ launch: TerminalLaunch) {
        guard !isRunning else { return }
        isRunning = true

        let theme = TerminalTheme(light: Self.colors, dark: Self.colors)
        let controller = TerminalController(theme: theme) { builder in
            // Match the product Host's proven libghostty launch path: EXEC's
            // command is part of the controller config, while per-session
            // cwd/environment stay on the surface options below.
            builder.withCustom("command", launch.executable)
            builder.withCustom("wait-after-command", "false")
            builder.withCustom("keybind", "clear")
            builder.withCustom("keybind", "performable:super+c=copy_to_clipboard")
            builder.withCustom("keybind", "super+v=paste_from_clipboard")
            builder.withCustom("shell-integration", "none")
            builder.withCustom("window-padding-balance", "false")
            builder.withCustom("window-padding-color", "extend")
            builder.withWindowPaddingX(0)
            builder.withWindowPaddingY(0)
            builder.withCursorStyle(.block)
            builder.withCursorStyleBlink(true)
            // Default-background cells reveal an optional local Surface
            // CAMetalLayer behind Ghostty. Non-Surface sessions still see the
            // TerminalCard's ordinary opaque background.
            builder.withBackgroundOpacity(0)
            // Ghostty config files always require a dot decimal separator;
            // the typed formatter in libghostty-spm currently follows the
            // user's locale (and emits `12,5` under Norwegian locales).
            builder.withCustom("font-size", "12.5")
        }
        if ProcessInfo.processInfo.environment["UNPEEL_KITCHEN_GHOSTTY_DEBUG"] == "1" {
            let issue = controller.lastConfigurationIssue ?? "none"
            let resources = GhosttyRuntimeResources.directoryURL?.path ?? "missing"
            let terminfo = GhosttyRuntimeResources.terminfoDirectoryURL?.path ?? "missing"
            NSLog(
                "[KitchenSink] Ghostty issue=%@ resources=%@ terminfo=%@ command=%@ cwd=%@",
                issue,
                resources,
                terminfo,
                launch.executable,
                launch.currentDirectory
            )
        }
        let terminal = KitchenSinkTerminalView(frame: view.bounds)
        terminal.configuration = TerminalSurfaceOptions(
            backend: .exec,
            fontSize: 12.5,
            workingDirectory: launch.currentDirectory,
            envVars: launch.environment,
            context: .window
        )
        terminal.delegate = self
        terminal.controller = controller
        terminalController = controller
        terminalView = terminal
        view.install(terminal)
        // libghostty creates the exec backend with its first visible surface.
        // Bootstrap that surface for one main-loop turn even when this session
        // starts in the parking window; otherwise an unselected session never
        // launches its PTY, never creates ui.sock, and cannot participate in
        // the semantic audit. Parked sessions are hidden again immediately,
        // so only their always-on process remains active.
        terminal.setSurfaceVisible(true)
        if !isDisplayed {
            DispatchQueue.main.async { [weak self, weak terminal] in
                guard let self, let terminal,
                      self.terminalView === terminal,
                      !self.isDisplayed
                else { return }
                terminal.setSurfaceVisible(false)
            }
        }
    }

    func terminate() {
        guard isRunning else { return }
        isRunning = false
        tearDownSurface()
        DispatchQueue.main.async { [weak self] in
            self?.onTermination?(nil)
        }
    }

    func takeForDisplay() -> KitchenSinkTerminalHostView {
        isDisplayed = true
        view.removeFromSuperview()
        terminalView?.setSurfaceVisible(true)
        return view
    }

    func park() {
        isDisplayed = false
        terminalView?.setSurfaceVisible(false)
        guard view.superview !== parkingView else { return }
        view.removeFromSuperview()
        view.translatesAutoresizingMaskIntoConstraints = true
        view.frame = parkingView.bounds
        view.autoresizingMask = [.width, .height]
        parkingView.addSubview(view)
    }

    func requestFocus() {
        guard isDisplayed else { return }
        view.requestFocus()
    }

    private func tearDownSurface() {
        terminalView?.delegate = nil
        terminalView?.setSurfaceVisible(false)
        terminalView?.controller = nil
        view.install(nil)
        terminalView = nil
        terminalController = nil
    }

    private static let colors = TerminalConfiguration { builder in
        builder.withBackground("0E1014")
        builder.withForeground("DBE0E8")
        builder.withCursorColor("35C2B4")
        builder.withCursorText("0E1014")
        builder.withSelectionBackground("2F4E78")
        builder.withSelectionForeground("FFFFFF")
        builder.withPalette(0, color: "15191F")
        builder.withPalette(1, color: "E06C75")
        builder.withPalette(2, color: "98C379")
        builder.withPalette(3, color: "E5C07B")
        builder.withPalette(4, color: "61AFEF")
        builder.withPalette(5, color: "C678DD")
        builder.withPalette(6, color: "56B6C2")
        builder.withPalette(7, color: "D7DAE0")
        builder.withPalette(8, color: "5C6370")
        builder.withPalette(9, color: "E06C75")
        builder.withPalette(10, color: "98C379")
        builder.withPalette(11, color: "E5C07B")
        builder.withPalette(12, color: "61AFEF")
        builder.withPalette(13, color: "C678DD")
        builder.withPalette(14, color: "56B6C2")
        builder.withPalette(15, color: "FFFFFF")
    }
}

extension TerminalEngineController: TerminalSurfaceCloseDelegate {
    func terminalDidClose(processAlive _: Bool) {
        guard isRunning else { return }
        isRunning = false
        tearDownSurface()
        onTermination?(nil)
    }
}

struct TerminalPane: NSViewRepresentable {
    let engine: TerminalEngineController
    let autoFocus: Bool

    final class Coordinator {
        let engine: TerminalEngineController
        var autoFocus: Bool

        init(engine: TerminalEngineController, autoFocus: Bool) {
            self.engine = engine
            self.autoFocus = autoFocus
        }
    }

    func makeCoordinator() -> Coordinator {
        Coordinator(engine: engine, autoFocus: autoFocus)
    }

    func makeNSView(context _: Context) -> KitchenSinkTerminalHostView {
        let view = engine.takeForDisplay()
        if autoFocus { engine.requestFocus() }
        return view
    }

    func updateNSView(_: KitchenSinkTerminalHostView, context: Context) {
        let becameAutoFocused = autoFocus && !context.coordinator.autoFocus
        context.coordinator.autoFocus = autoFocus
        if becameAutoFocused { engine.requestFocus() }
    }

    static func dismantleNSView(
        _: KitchenSinkTerminalHostView,
        coordinator: Coordinator
    ) {
        coordinator.engine.park()
    }
}
