import AppKit
import Testing
@testable import UnpeelNative

@Suite(.serialized)
@MainActor
struct TerminalPaneWindowTests {
    private final class StaleEventWindow: NSWindow {
        var staleEvent: NSEvent?
        override var currentEvent: NSEvent? { staleEvent }
    }

    private final class MouseReceiver: NSView {
        var received: [NSEvent] = []
        var onMouseDown: (() -> Void)?
        override var acceptsFirstResponder: Bool { true }
        override func acceptsFirstMouse(for event: NSEvent?) -> Bool { true }
        override func mouseDown(with event: NSEvent) {
            onMouseDown?()
            received.append(event)
        }
        override func rightMouseDown(with event: NSEvent) { mouseDown(with: event) }
        override func otherMouseDown(with event: NSEvent) { mouseDown(with: event) }
    }

    private func configure(_ window: NSWindow) {
        window.isReleasedWhenClosed = false
        window.contentView = NSView(frame: CGRect(x: 0, y: 0, width: 400, height: 300))
        // AppKit does not deliver mouse events to unordered windows. Keep
        // this one offscreen and behind the user's windows without focusing it.
        window.setFrameOrigin(CGPoint(x: -10000, y: -10000))
        window.orderBack(nil)
    }

    private func pane(remote: Bool) -> NSView & TerminalPaneActivating {
        let frame = CGRect(x: 40, y: 30, width: 200, height: 180)
        if remote {
            return RemoteTerminalPaneHostView.SwapContainer(frame: frame)
        }
        return TerminalHostView.SwapContainer(frame: frame)
    }

    private func onActivate(_ view: NSView, _ callback: @escaping () -> Void) {
        if let view = view as? TerminalHostView.SwapContainer {
            view.onActivate = callback
        } else if let view = view as? RemoteTerminalPaneHostView.SwapContainer {
            view.onActivate = callback
        }
    }

    private func event(
        in window: NSWindow,
        type: NSEvent.EventType = .leftMouseDown,
        location: CGPoint = CGPoint(x: 100, y: 100)
    ) throws -> NSEvent {
        try #require(NSEvent.mouseEvent(
            with: type, location: location, modifierFlags: [],
            timestamp: ProcessInfo.processInfo.systemUptime,
            windowNumber: window.windowNumber, context: nil,
            eventNumber: 1, clickCount: 1, pressure: 1
        ))
    }

    @Test(arguments: [false, true])
    func workspaceLayoutQueriesNeverActivateWithAStaleClick(remote: Bool) throws {
        _ = NSApplication.shared
        let window = StaleEventWindow(
            contentRect: CGRect(x: 0, y: 0, width: 400, height: 300),
            styleMask: .borderless, backing: .buffered, defer: false
        )
        configure(window)
        defer { window.close() }
        window.staleEvent = try event(in: window)
        var activations = 0

        // Replacing workspaces while AppKit still remembers the last press
        // must be a read-only operation, even with repeated layout hit tests.
        for _ in 0..<30 {
            let mount = pane(remote: remote)
            onActivate(mount) { activations += 1 }
            window.contentView?.addSubview(mount)
            for _ in 0..<10 {
                #expect(window.contentView?.hitTest(CGPoint(x: 100, y: 100)) === mount)
            }
            mount.removeFromSuperview()
        }
        #expect(activations == 0)
    }

    @Test(arguments: [false, true])
    func pressesActivateOnceBeforeReachingTheTerminal(remote: Bool) throws {
        _ = NSApplication.shared
        let window = TerminalPaneWindow(
            contentRect: CGRect(x: 0, y: 0, width: 400, height: 300),
            styleMask: .borderless, backing: .buffered, defer: false
        )
        configure(window)
        defer { window.close() }
        let mount = pane(remote: remote)
        let receiver = MouseReceiver(frame: mount.bounds)
        mount.addSubview(receiver)
        window.contentView?.addSubview(mount)
        var activations = 0
        onActivate(mount) { activations += 1 }
        receiver.onMouseDown = {
            #expect(activations == receiver.received.count + 1)
            for _ in 0..<20 {
                _ = window.contentView?.hitTest(CGPoint(x: 100, y: 100))
            }
        }
        defer { receiver.onMouseDown = nil }

        for type in [NSEvent.EventType.leftMouseDown, .rightMouseDown, .otherMouseDown] {
            let press = try event(in: window, type: type)
            window.sendEvent(press)
            #expect(receiver.received.last === press)
            #expect(activations == receiver.received.count)
        }
        #expect(activations == 3)
        window.sendEvent(try event(in: window, type: .mouseMoved))
        #expect(activations == 3)
    }

    @Test(arguments: [false, true])
    func onlyTheCurrentUncoveredPaneCanActivate(remote: Bool) throws {
        _ = NSApplication.shared
        let window = TerminalPaneWindow(
            contentRect: CGRect(x: 0, y: 0, width: 400, height: 300),
            styleMask: .borderless, backing: .buffered, defer: false
        )
        configure(window)
        defer { window.close() }
        let old = pane(remote: remote)
        let current = pane(remote: remote)
        old.addSubview(MouseReceiver(frame: old.bounds))
        current.addSubview(MouseReceiver(frame: current.bounds))
        var oldActivations = 0
        var currentActivations = 0
        onActivate(old) { oldActivations += 1 }
        onActivate(current) { currentActivations += 1 }
        window.contentView?.addSubview(old)
        window.contentView?.addSubview(current)
        window.sendEvent(try event(in: window))
        #expect(oldActivations == 0)
        #expect(currentActivations == 1)

        old.removeFromSuperview()
        current.isHidden = true
        window.sendEvent(try event(in: window))
        #expect(currentActivations == 1)
        current.isHidden = false

        // A sibling overlay (for example a workspace/approval UI) owns its
        // clicks; the terminal below must not change focus.
        let overlay = MouseReceiver(frame: current.frame)
        window.contentView?.addSubview(overlay)
        window.sendEvent(try event(in: window))
        #expect(overlay.received.count == 1)
        #expect(currentActivations == 1)

        overlay.removeFromSuperview()
        window.sendEvent(try event(in: window, location: CGPoint(x: 350, y: 250)))
        #expect(currentActivations == 1)
        window.sendEvent(try event(in: window))
        #expect(currentActivations == 2)
        #expect(oldActivations == 0)
    }
}
