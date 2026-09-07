import AppKit

/// Implemented by both terminal transports' retained mount views.
@MainActor
protocol TerminalPaneActivating: AnyObject {
    var onActivate: (() -> Void)? { get }
}

/// Pane focus follows dispatched mouse presses. AppKit also hit-tests while
/// SwiftUI lays out and replaces workspace views, with the previous click
/// still in `currentEvent`. Publishing focus from those queries re-enters
/// the view update and can repeatedly activate panes during a workspace swap.
final class TerminalPaneWindow: NSWindow {
    override func sendEvent(_ event: NSEvent) {
        if event.window === self, attachedSheet == nil,
           event.type == .leftMouseDown
            || event.type == .rightMouseDown
            || event.type == .otherMouseDown,
           let contentView {
            // hitTest takes a point in the receiver's SUPERview coordinates.
            // Resolve the actual frontmost target before publishing anything;
            // covered, hidden, and detached workspace panes cannot claim it.
            let point = contentView.superview?.convert(event.locationInWindow, from: nil)
                ?? event.locationInWindow
            var target = contentView.hitTest(point)
            while let view = target {
                if let pane = view as? any TerminalPaneActivating {
                    pane.onActivate?()
                    break
                }
                if view === contentView { break }
                target = view.superview
            }
        }
        // Preserve the original event and normal terminal mouse handling,
        // including selection, context menus, command-click, and file drags.
        super.sendEvent(event)
    }
}
