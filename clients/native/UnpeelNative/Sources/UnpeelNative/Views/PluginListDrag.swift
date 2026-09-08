import AppKit
import SwiftUI

/// The sidebar's detached-card interaction, scoped to a settings list. Cursor
/// movement stays in AppKit; SwiftUI publishes only insertion-slot changes.
@MainActor
final class PluginListDragController: ObservableObject {
    @Published private(set) var draggedID: String?
    @Published private(set) var targetIndex = 0
    private final class WeakView {
        weak var view: NSView?
        init(_ view: NSView) { self.view = view }
    }
    private struct Press {
        let id: String
        let screenPoint: NSPoint
    }
    private var rows: [String: WeakView] = [:]
    private var exclusions: [ObjectIdentifier: WeakView] = [:]
    private weak var container: NSView?
    private var monitor: Any?
    private var timer: Timer?
    private var press: Press?
    private var card: NSWindow?
    private var cardState: SidebarSessionDragCardState?
    private var frozenIDs: [String] = []
    private var frozenFrames: [CGRect] = []
    private var sourceIndex = 0
    private var sourceHeight: CGFloat = 0
    private var grabOffset = NSPoint.zero
    private var grabYInRow: CGFloat = 0
    private var settling = false
    private var settleWork: DispatchWorkItem?
    private let margin = SidebarSessionDragController.cardWindowMargin
    private var ids: [String] = []
    private var enabled = false
    private var preview: ((String) -> AnyView)?
    private var commit: (([String]) -> Void)?

    func bind(view: NSView, ids: [String], enabled: Bool,
              preview: @escaping (String) -> AnyView, commit: @escaping ([String]) -> Void) {
        if draggedID != nil, self.ids != ids, !settling { endDrag(cancelled: true) }
        container = view
        self.ids = ids
        self.enabled = enabled
        self.preview = preview
        self.commit = commit
        if monitor == nil {
            monitor = NSEvent.addLocalMonitorForEvents(matching: [.leftMouseDown, .leftMouseDragged, .leftMouseUp, .keyDown]) { [weak self] event in
                let keep = MainActor.assumeIsolated { self.map { $0.handle(event) != nil } ?? true }
                return keep ? event : nil
            }
        }
    }
    func register(_ view: NSView, id: String?) {
        if let id { rows[id] = WeakView(view) }
        else { exclusions[ObjectIdentifier(view)] = WeakView(view) }
    }
    func unregister(_ view: NSView, id: String?) {
        if let id, rows[id]?.view === view { rows.removeValue(forKey: id) }
        exclusions.removeValue(forKey: ObjectIdentifier(view))
    }
    func detach() {
        if let monitor { NSEvent.removeMonitor(monitor) }
        monitor = nil
        timer?.invalidate(); timer = nil
        settleWork?.cancel(); settleWork = nil
        card?.orderOut(nil); card = nil; cardState = nil
        press = nil; draggedID = nil; settling = false
        container = nil; preview = nil; commit = nil
    }
    func offset(for id: String) -> CGFloat {
        guard draggedID != nil, let index = frozenIDs.firstIndex(of: id) else { return 0 }
        return Self.slotOffset(index: index, source: sourceIndex, target: targetIndex, stride: sourceHeight + 16)
    }
    nonisolated static func slotOffset(index: Int, source: Int, target: Int, stride: CGFloat) -> CGFloat {
        if source < target && index > source && index <= target { return -stride }
        if source > target && index >= target && index < source { return stride }
        return 0
    }
    nonisolated static func reordered(_ ids: [String], source: Int, target: Int) -> [String] {
        guard ids.indices.contains(source), ids.indices.contains(target) else { return ids }
        var result = ids
        let id = result.remove(at: source)
        result.insert(id, at: target)
        return result
    }
    func handle(_ event: NSEvent) -> NSEvent? {
        if event.type == .keyDown, event.keyCode == 53, press != nil || draggedID != nil {
            endDrag(cancelled: true); return nil
        }
        guard let container, event.window === container.window else { return event }
        switch event.type {
        case .leftMouseDown:
            guard enabled, draggedID == nil, !settling, !event.modifierFlags.contains(.control) else { return event }
            if let scroll = container.enclosingScrollView,
               !scroll.contentView.bounds.contains(scroll.contentView.convert(event.locationInWindow, from: nil)) { return event }
            exclusions = exclusions.filter { $0.value.view != nil }
            if exclusions.values.contains(where: { entry in
                guard let view = entry.view, view.window === event.window else { return false }
                return view.bounds.contains(view.convert(event.locationInWindow, from: nil))
            }) { return event }
            guard let id = ids.first(where: { id in
                guard let view = rows[id]?.view else { return false }
                return view.bounds.contains(view.convert(event.locationInWindow, from: nil))
            }) else { return event }
            press = Press(id: id, screenPoint: NSEvent.mouseLocation)
            startPolling()
            // Row chrome has no click action. Own the press immediately so
            // AppKit cannot start a competing control/window tracking loop.
            return nil
        case .leftMouseDragged:
            poll()
            if press != nil || draggedID != nil { return nil }
        case .leftMouseUp:
            guard NSEvent.pressedMouseButtons & 1 == 0 else { return event }
            let lifted = draggedID != nil
            endDrag(cancelled: false)
            if lifted { return nil }
        default: break
        }
        return event
    }
    private func startPolling() {
        guard timer == nil else { return }
        let timer = Timer(timeInterval: 1 / 60, repeats: true) { [weak self] _ in
            MainActor.assumeIsolated { self?.poll() }
        }
        self.timer = timer
        RunLoop.main.add(timer, forMode: .common)
    }
    private func poll() {
        guard !settling else { return }
        guard NSEvent.pressedMouseButtons & 1 != 0 else { endDrag(cancelled: false); return }
        let screen = NSEvent.mouseLocation
        if draggedID == nil, let press,
           hypot(screen.x - press.screenPoint.x, screen.y - press.screenPoint.y) >= SidebarSessionDragController.dragThreshold {
            beginDrag(id: press.id, at: press.screenPoint)
        }
        updateDrag(at: screen)
    }
    func updateDrag(at screen: NSPoint) {
        guard !settling else { return }
        guard let card, let container, let window = container.window, draggedID != nil else { return }
        let origin = NSPoint(x: screen.x - grabOffset.x, y: screen.y - grabOffset.y)
        if card.frame.origin != origin { card.setFrameOrigin(origin) }
        autoScroll(screen: screen)
        let point = container.convert(window.convertPoint(fromScreen: screen), from: nil)
        // Compare the card's centre with the other rows' centres, retaining the
        // initial grab offset even when a row contains several command fields.
        let centreY = point.y - grabYInRow + sourceHeight / 2
        let next = frozenFrames.enumerated().filter { $0.offset != sourceIndex && centreY > $0.element.midY }.count
        if targetIndex != next { targetIndex = next }
    }
    func beginDrag(id: String, at screenPoint: NSPoint) {
        guard enabled, draggedID == nil, !settling else { return }
        let press = Press(id: id, screenPoint: screenPoint)
        guard let container, let window = container.window, let index = ids.firstIndex(of: press.id),
              let source = rows[press.id]?.view, let preview else { return }
        frozenIDs = ids
        frozenFrames = ids.compactMap { rows[$0]?.view.map { container.convert($0.bounds, from: $0) } }
        guard frozenFrames.count == ids.count else { return }
        sourceIndex = index; targetIndex = index
        let frame = frozenFrames[index]
        sourceHeight = frame.height
        grabYInRow = container.convert(window.convertPoint(fromScreen: press.screenPoint), from: nil).y - frame.minY
        let screenFrame = window.convertToScreen(source.convert(source.bounds, to: nil))
        let cardFrame = screenFrame.insetBy(dx: -margin, dy: -margin)
        grabOffset = NSPoint(x: press.screenPoint.x - cardFrame.minX, y: press.screenPoint.y - cardFrame.minY)
        let floating = NSPanel(contentRect: cardFrame, styleMask: [.borderless, .nonactivatingPanel], backing: .buffered, defer: false)
        floating.isOpaque = false; floating.backgroundColor = .clear
        floating.hasShadow = false; floating.ignoresMouseEvents = true
        floating.level = window.level; floating.isReleasedWhenClosed = false
        floating.animationBehavior = .none
        floating.appearance = window.appearance ?? NSApp.effectiveAppearance
        let state = SidebarSessionDragCardState()
        let content = PluginDragCardView(content: preview(press.id), state: state,
                                         size: frame.size, margin: margin)
        let host = NSHostingView(rootView: content)
        host.frame = NSRect(origin: .zero, size: cardFrame.size)
        floating.contentView = host
        card = floating; cardState = state
        draggedID = press.id
        floating.order(.above, relativeTo: window.windowNumber)
        withAnimation(SidebarMotion.reduceMotion ? nil : SidebarSessionDragController.liftAnimation) {
            state.lifted = true
        }
    }
    private func autoScroll(screen: NSPoint) {
        guard let container, let window = container.window, let scroll = container.enclosingScrollView,
              let document = scroll.documentView else { return }
        let clip = scroll.contentView
        let point = clip.convert(window.convertPoint(fromScreen: screen), from: nil)
        let zone = SidebarSessionDragController.autoScrollZone
        let top = point.y - clip.bounds.minY, bottom = clip.bounds.maxY - point.y
        let direction: CGFloat = top < zone ? -max(0, 1 - top / zone) : (bottom < zone ? max(0, 1 - bottom / zone) : 0)
        guard direction != 0 else { return }
        var origin = clip.bounds.origin
        origin.y = min(max(0, origin.y + min(1, max(-1, direction)) * SidebarSessionDragController.autoScrollMaxStep), max(0, document.bounds.height - clip.bounds.height))
        guard origin != clip.bounds.origin else { return }
        clip.scroll(to: origin); scroll.reflectScrolledClipView(clip)
    }
    nonisolated static func destinationFrame(in frames: [CGRect], source: Int, target: Int) -> CGRect? {
        guard frames.indices.contains(source), frames.indices.contains(target) else { return nil }
        var frame = frames[source]
        frame.origin.y = source < target ? frames[target].maxY - frame.height : frames[target].minY
        return frame
    }

    func endDrag(cancelled: Bool) {
        timer?.invalidate(); timer = nil
        guard draggedID != nil, !settling else { press = nil; return }
        settling = true
        if cancelled { targetIndex = sourceIndex }
        press = nil
        let order = Self.reordered(frozenIDs, source: sourceIndex, target: targetIndex)
        let shouldCommit = !cancelled && sourceIndex != targetIndex
        if let state = cardState {
            withAnimation(SidebarMotion.reduceMotion ? nil : SidebarSessionDragController.settleAnimation) {
                state.lifted = false
            }
        }
        // The gap stays in place during the flight. Its geometry is known
        // already; do not commit/rebuild the list and then chase a row frame
        // that may still be moving through SwiftUI layout.
        if let card, let container, let window = container.window,
           let frame = Self.destinationFrame(in: frozenFrames, source: sourceIndex, target: targetIndex) {
            let target = window.convertToScreen(container.convert(frame, to: nil))
                .insetBy(dx: -margin, dy: -margin)
            NSAnimationContext.runAnimationGroup { context in
                context.duration = SidebarMotion.reduceMotion ? 0 : SidebarSessionDragController.settleDelay
                context.timingFunction = SidebarSessionDragController.settleTimingFunction()
                card.animator().setFrame(target, display: true)
            }
        }
        let work = DispatchWorkItem { [weak self] in
            guard let self, self.settling else { return }
            var transaction = Transaction(); transaction.disablesAnimations = true
            withTransaction(transaction) {
                if shouldCommit { self.commit?(order) }
                self.draggedID = nil
                self.settling = false
            }
            let landedCard = self.card
            self.card = nil; self.cardState = nil; self.settleWork = nil
            // Keep the landed image covering the slot until SwiftUI has
            // applied the atomic order/offset swap on the next run-loop turn.
            DispatchQueue.main.async { landedCard?.orderOut(nil) }
        }
        settleWork = work
        DispatchQueue.main.asyncAfter(deadline: .now() + (SidebarMotion.reduceMotion ? 0 : SidebarSessionDragController.settleDelay), execute: work)
    }
}

private struct PluginDragCardView: View {
    let content: AnyView
    @ObservedObject var state: SidebarSessionDragCardState
    let size: CGSize
    let margin: CGFloat
    var body: some View {
        content.frame(width: size.width, height: size.height)
            .background(Color(nsColor: Theme.terminalBackgroundNSColor.withAlphaComponent(1)),
                        in: RoundedRectangle(cornerRadius: 11))
            .compositingGroup()
            .scaleEffect(state.lifted && !SidebarMotion.reduceMotion ? SidebarSessionDragController.liftScale : 1)
            .shadow(color: .black.opacity(state.lifted ? 0.30 : 0),
                    radius: state.lifted ? 16 : 3, x: 0, y: state.lifted ? 8 : 1)
            .padding(margin)
            .allowsHitTesting(false)
    }
}

private final class PluginDragPassiveView: NSView {
    override var isFlipped: Bool { true }
    override func hitTest(_ point: NSPoint) -> NSView? { nil }
}

struct PluginDragAnchor: NSViewRepresentable {
    let controller: PluginListDragController
    let id: String
    func makeCoordinator() -> Coordinator { Coordinator(controller: controller, id: id) }
    func makeNSView(context: Context) -> NSView {
        let view = PluginDragPassiveView(); controller.register(view, id: id); return view
    }
    func updateNSView(_ view: NSView, context: Context) { controller.register(view, id: id) }
    static func dismantleNSView(_ view: NSView, coordinator: Coordinator) { coordinator.controller.unregister(view, id: coordinator.id) }
    final class Coordinator {
        let controller: PluginListDragController
        let id: String
        init(controller: PluginListDragController, id: String) { self.controller = controller; self.id = id }
    }
}
struct PluginDragExclusion: NSViewRepresentable {
    let controller: PluginListDragController
    func makeCoordinator() -> PluginListDragController { controller }
    func makeNSView(context: Context) -> NSView {
        let view = PluginDragPassiveView(); controller.register(view, id: nil); return view
    }
    func updateNSView(_ view: NSView, context: Context) { controller.register(view, id: nil) }
    static func dismantleNSView(_ view: NSView, coordinator: PluginListDragController) { coordinator.unregister(view, id: nil) }
}
struct PluginDragMonitor: NSViewRepresentable {
    let controller: PluginListDragController
    let ids: [String]
    let enabled: Bool
    let preview: (String) -> AnyView
    let commit: ([String]) -> Void
    func makeCoordinator() -> PluginListDragController { controller }
    func makeNSView(context: Context) -> NSView { PluginDragPassiveView() }
    func updateNSView(_ view: NSView, context: Context) {
        controller.bind(view: view, ids: ids, enabled: enabled, preview: preview, commit: commit)
    }
    static func dismantleNSView(_ view: NSView, coordinator: PluginListDragController) { coordinator.detach() }
}
struct PluginRowDragEffects: ViewModifier {
    @ObservedObject var controller: PluginListDragController
    let id: String
    func body(content: Content) -> some View {
        content.opacity(controller.draggedID == id ? 0 : 1)
            .allowsHitTesting(controller.draggedID == nil)
            .offset(y: controller.offset(for: id))
            .animation(SidebarMotion.reduceMotion ? nil : SidebarSessionDragController.slotAnimation, value: controller.targetIndex)
    }
}
