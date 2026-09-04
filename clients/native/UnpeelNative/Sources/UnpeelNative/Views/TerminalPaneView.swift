//
//  TerminalPaneView.swift
//  UnpeelNative
//
//  Controller-window pane presentation. A PaneGroup is viewer state only:
//  every Session keeps running independently on its Host, while this view
//  renders the group's recursive split tree — mixed horizontal/vertical
//  splits of terminal or transient launcher panes.
//
//  Layout changes are deliberately immediate. Adding, detaching, swapping,
//  and resizing never animate geometry around Metal-backed terminal surfaces,
//  and inserted pane content appears in place with no entrance motion.
//

import AppKit
import SwiftUI
import UnpeelShared

extension Notification.Name {
    /// AppDelegate's scoped ⌘W request. The active TerminalPaneContainer
    /// resolves its focused leaf and owns any in-content confirmation.
    static let unpeelCloseActivePane = Notification.Name(
        "unpeel.terminal-pane.close-active"
    )
}

// MARK: - Drop zones

/// Pixel geometry shared by the split renderer and its drop preview. A
/// group-edge insert gives the arriving Session `1/(existing + 1)` of the
/// root, after reserving the same divider gap the final layout will use.
enum TerminalPaneDropPreviewGeometry {
    static let dividerWidth: CGFloat = 8
    static let previewInset: CGFloat = 6

    static func groupEdgePaneExtent(
        totalExtent: CGFloat,
        existingSessionLeafCount: Int
    ) -> CGFloat {
        let available = max(0, totalExtent - dividerWidth)
        let existing = max(1, existingSessionLeafCount)
        return available / CGFloat(existing + 1)
    }

    static func insetHighlightExtent(for paneExtent: CGFloat) -> CGFloat {
        max(0, paneExtent - previewInset * 2)
    }
}

/// Closing is a Session lifecycle action, deliberately distinct from the
/// presentation-only "Detach Pane" verb. Empty launchers just disappear;
/// terminals without resumable provider state are disposable, while an agent
/// conversation is stopped and archived only after confirmation.
enum TerminalPaneCloseAction: Equatable {
    case detachPane
    case removeSession(String)
    case confirmArchive(String)
}

func terminalPaneCloseAction(
    for content: PaneContent,
    canArchiveSession: Bool
) -> TerminalPaneCloseAction {
    guard let sessionID = content.sessionID else { return .detachPane }
    return canArchiveSession
        ? .confirmArchive(sessionID)
        : .removeSession(sessionID)
}

/// Highlight while an eligible Session drag hovers a drop target: a band at
/// one of the four content edges (group-edge split), half of a specific pane
/// (pane-edge split), or the distinct Pin square that creates an empty right
/// project sidebar's first member.
struct TerminalPaneDropZonesOverlay: View {
    @ObservedObject var store: UnpeelStore
    @Environment(\.sidebarSessionDragController) private var dragController

    var body: some View {
        GeometryReader { geo in
            ZStack {
                if let target = store.terminalPaneDropTarget {
                    let preview = TerminalPaneDropZonePreview(
                        target: target,
                        size: geo.size,
                        existingSessionLeafCount: existingSessionLeafCount
                    )
                    // The preview mirrors the card's magnet and leans toward
                    // the chip. Its per-move offset lives on a tiny state
                    // object only this leaf observes, so the churn never
                    // re-renders the store's observers.
                    if let magnet = dragController?.dropZoneMagnet {
                        TerminalPaneDropZoneMagnetLean(magnet: magnet) {
                            preview
                        }
                        .transition(Self.zoneTransition)
                        .zIndex(0)
                    } else {
                        preview
                            .transition(Self.zoneTransition)
                            .zIndex(0)
                    }
                }
                if let pinState = dragController?.projectSidebarPinDropState {
                    ProjectSidebarPinDropTarget(
                        state: pinState,
                        availableSize: geo.size
                    )
                        .zIndex(10)
                }
            }
            .animation(
                SidebarMotion.reduceMotion
                    ? .easeOut(duration: 0.12)
                    : .spring(response: 0.34, dampingFraction: 0.72),
                value: store.terminalPaneDropTarget
            )
        }
        .allowsHitTesting(false)
    }

    /// Mirrors `PaneLayoutState.insertSession(atGroupEdge:)`. A solo
    /// terminal counts as one existing leaf; an established group uses its
    /// durable Session-leaf count (launchers do not affect the insert ratio).
    private var existingSessionLeafCount: Int {
        guard let selectedSessionID = store.selectedSessionID,
              let group = store.paneLayoutState.group(
                  containingSession: selectedSessionID
              )
        else { return 1 }
        return max(1, group.root.sessionLeaves.count)
    }

    /// Springy pop-in (the zone "arrives" for the chip); plain fade under
    /// reduce motion, and a quick fade out either way — an exit bounce
    /// under a just-dropped card would read as clutter.
    private static var zoneTransition: AnyTransition {
        if SidebarMotion.reduceMotion { return .opacity }
        return .asymmetric(
            insertion: .scale(scale: 0.94).combined(with: .opacity),
            removal: .opacity.animation(.easeOut(duration: 0.12))
        )
    }
}

/// Unlike the neutral split previews, pinning changes sidebar organization.
/// A compact green square and centered push-pin make that different action
/// explicit; hovering morphs it into the exact pane footprint that will
/// occupy the full-height project sidebar after release.
private struct ProjectSidebarPinDropTarget: View {
    @ObservedObject var state: ProjectSidebarPinDropState
    let availableSize: CGSize

    var body: some View {
        ZStack {
            if state.isAvailable {
                let expanded = state.isTargeted
                let shape = RoundedRectangle(
                    cornerRadius: expanded ? Theme.contentCornerRadius : 13,
                    style: .continuous
                )
                shape
                    .fill(Theme.accent.opacity(state.isTargeted ? 0.2 : 0.07))
                    .overlay(
                        shape.stroke(
                            Theme.accent.opacity(state.isTargeted ? 0.75 : 0.3),
                            lineWidth: state.isTargeted ? 1.5 : 1
                        )
                    )
                    .overlay {
                        ChromeIconView(icon: .pushPin, size: 22)
                            .foregroundStyle(Theme.accent)
                            .opacity(state.isTargeted ? 1 : 0.55)
                            .offset(
                                x: state.cursorLean.width,
                                y: state.cursorLean.height
                            )
                    }
                    .frame(
                        width: expanded
                            ? min(availableSize.width, state.previewPaneWidth)
                            : SidebarSessionDragController.projectSidebarPinTargetSize,
                        height: expanded
                            ? availableSize.height
                            : SidebarSessionDragController.projectSidebarPinTargetSize
                    )
                    .padding(
                        .trailing,
                        expanded
                            ? 0
                            : SidebarSessionDragController
                                .projectSidebarPinTargetTrailingInset
                    )
                    .padding(
                        .top,
                        expanded
                            ? 0
                            : SidebarSessionDragController.projectSidebarPinTargetTopInset
                    )
                    .frame(
                        maxWidth: .infinity,
                        maxHeight: .infinity,
                        alignment: .topTrailing
                    )
                    .scaleEffect(state.isTargeted ? 1 : 0.92)
                    .transition(
                        SidebarMotion.reduceMotion
                            ? .opacity
                            : .scale(scale: 0.9).combined(with: .opacity)
                    )
            }
        }
        .animation(
            SidebarMotion.reduceMotion
                ? .easeOut(duration: 0.12)
                : .spring(response: 0.3, dampingFraction: 0.72),
            value: state.isTargeted
        )
        .animation(
            SidebarMotion.reduceMotion
                ? .easeOut(duration: 0.12)
                : .spring(response: 0.3, dampingFraction: 0.72),
            value: state.isAvailable
        )
        .animation(
            SidebarMotion.reduceMotion
                ? nil
                : .spring(response: 0.24, dampingFraction: 0.74),
            value: state.cursorLean
        )
    }
}

private struct TerminalPaneDropZonePreview: View {
    let target: PaneDropTarget
    let size: CGSize
    let existingSessionLeafCount: Int

    var body: some View {
        let shape = RoundedRectangle(cornerRadius: 10, style: .continuous)
        let band = shape
            .fill(Theme.hoverRow)
            .overlay(
                shape.stroke(Theme.foreground.opacity(0.14), lineWidth: 1)
            )
        // Pane-edge highlights render inside each leaf; the area overlay only
        // draws group-edge previews. The hit band stays deliberately narrow,
        // but the preview occupies the footprint the arriving terminal will
        // receive after the split.
        switch target {
        case .pane:
            band.frame(width: 0, height: 0).opacity(0)
        case let .groupEdge(edge):
            let totalExtent = edge == .left || edge == .right
                ? size.width
                : size.height
            let paneExtent = TerminalPaneDropPreviewGeometry.groupEdgePaneExtent(
                totalExtent: totalExtent,
                existingSessionLeafCount: existingSessionLeafCount
            )
            let highlightExtent = TerminalPaneDropPreviewGeometry
                .insetHighlightExtent(for: paneExtent)
            let inset = TerminalPaneDropPreviewGeometry.previewInset
            switch edge {
            case .left, .right:
                band
                    .frame(
                        width: highlightExtent,
                        height: max(0, size.height - inset * 2)
                    )
                    .padding(inset)
                    .frame(
                        maxWidth: .infinity,
                        alignment: edge == .left ? .leading : .trailing
                    )
            case .up, .down:
                band
                    .frame(
                        width: max(0, size.width - inset * 2),
                        height: highlightExtent
                    )
                    .padding(inset)
                    .frame(
                        maxHeight: .infinity,
                        alignment: edge == .up ? .top : .bottom
                    )
            }
        }
    }
}

/// Right-panel pane frame registration: the drag controller uses it for the
/// detached card's source geometry and as the hover target for live stack
/// reordering (`updatePanelReorder`). Click-through.
private struct PanelPaneDragSourceRegistration: NSViewRepresentable {
    let controller: SidebarSessionDragController
    let sessionID: String

    func makeCoordinator() -> Coordinator { Coordinator() }

    func makeNSView(context: Context) -> TargetView {
        let view = TargetView()
        context.coordinator.update(controller: controller, view: view, sessionID: sessionID)
        return view
    }

    func updateNSView(_ view: TargetView, context: Context) {
        context.coordinator.update(controller: controller, view: view, sessionID: sessionID)
    }

    static func dismantleNSView(_ view: TargetView, coordinator: Coordinator) {
        coordinator.remove()
    }

    final class TargetView: NSView {
        override func hitTest(_: NSPoint) -> NSView? { nil }
    }

    @MainActor
    final class Coordinator {
        private let token = UUID()
        private weak var controller: SidebarSessionDragController?
        private var sessionID: String?

        func update(
            controller: SidebarSessionDragController,
            view: NSView,
            sessionID: String
        ) {
            if let oldController = self.controller,
               let oldSessionID = self.sessionID,
               oldController !== controller || oldSessionID != sessionID {
                oldController.removePanelPaneSource(sessionID: oldSessionID, token: token)
            }
            self.controller = controller
            self.sessionID = sessionID
            controller.registerPanelPaneSource(view: view, sessionID: sessionID, token: token)
        }

        func remove() {
            if let sessionID {
                controller?.removePanelPaneSource(sessionID: sessionID, token: token)
            }
            controller = nil
            sessionID = nil
        }
    }
}

/// Supplies a live AppKit window-space frame for a mounted pane's content so
/// the drag controller can hit-test 4-sided pane-edge drops. Click-through.
private struct TerminalPaneDropTargetRegistration: NSViewRepresentable {
    let controller: SidebarSessionDragController
    let paneID: String
    let isSolo: Bool

    func makeCoordinator() -> Coordinator { Coordinator() }

    func makeNSView(context: Context) -> TargetView {
        let view = TargetView()
        context.coordinator.update(
            controller: controller, view: view, paneID: paneID, isSolo: isSolo
        )
        return view
    }

    func updateNSView(_ view: TargetView, context: Context) {
        context.coordinator.update(
            controller: controller, view: view, paneID: paneID, isSolo: isSolo
        )
    }

    static func dismantleNSView(_ view: TargetView, coordinator: Coordinator) {
        coordinator.remove()
    }

    final class TargetView: NSView {
        override func hitTest(_: NSPoint) -> NSView? { nil }
    }

    @MainActor
    final class Coordinator {
        private let token = UUID()
        private weak var controller: SidebarSessionDragController?
        private var paneID: String?

        func update(
            controller: SidebarSessionDragController,
            view: NSView,
            paneID: String,
            isSolo: Bool
        ) {
            if let oldController = self.controller,
               let oldPaneID = self.paneID,
               oldController !== controller || oldPaneID != paneID {
                oldController.removePaneDropTarget(paneID: oldPaneID, token: token)
            }
            self.controller = controller
            self.paneID = paneID
            controller.registerPaneDropTarget(
                view: view, paneID: paneID, isSolo: isSolo, token: token
            )
        }

        func remove() {
            if let paneID {
                controller?.removePaneDropTarget(paneID: paneID, token: token)
            }
            controller = nil
            paneID = nil
        }
    }
}

/// The only observer of the per-move square lean (see
/// `TerminalDropZoneMagnetState`). The spring makes the square PURSUE the
/// chip — each per-move offset retargets an in-flight spring, so the
/// square trails with a little organic lag and settle instead of tracking
/// the cursor rigidly.
private struct TerminalPaneDropZoneMagnetLean<Content: View>: View {
    @ObservedObject var magnet: TerminalDropZoneMagnetState
    @ViewBuilder var content: () -> Content

    var body: some View {
        content()
            .offset(x: magnet.offsetX)
            .animation(
                SidebarMotion.reduceMotion
                    ? nil
                    : .spring(response: 0.3, dampingFraction: 0.68),
                value: magnet.offsetX
            )
    }
}

// MARK: - Draggable pane title

/// The pane title is the drag handle. The shared sidebar drag controller
/// owns the detached card, hit-testing, and eventual destination. A solo
/// pane (nil group) has no siblings to reorder, but remains draggable so it
/// can become the first member of the project sidebar.
private struct TerminalPaneTitleChip: View {
    let session: SessionEntry
    let groupID: String?
    let paneID: String
    let isActive: Bool
    let isEditing: Bool
    /// Busy/starting/restarting: the chip's logo slot shows the tinted
    /// spinner instead of the runtime logo.
    let isWorking: Bool
    /// Runtime tint shared by the logo and the spinner.
    let tint: Color
    let dragController: SidebarSessionDragController?
    /// Right-panel pane: the chip starts an ordinary session drag (sort the
    /// panel, drop into the sidebar, or split into the main area) instead of
    /// the pane-title drag, which needs pane-layout membership.
    var isPanelPane = false
    let onBeginRename: () -> Void
    let onCommitRename: (String) -> Void
    let onEndRename: () -> Void

    @State private var hovering = false
    @State private var renameDraft = ""
    @FocusState private var renameFocused: Bool

    var body: some View {
        HStack(spacing: 5) {
            // The session's runtime mark lives INSIDE the button, so the
            // chip reads as one unit: logo + title (spinner while working).
            ZStack {
                if isWorking {
                    BrailleSpinner(color: tint)
                } else if session.status == .attention {
                    AttentionDot(color: Theme.attention)
                } else {
                    ToolIconView(command: session.presentationCommand, size: 13)
                        .foregroundStyle(tint)
                }
            }
            .frame(width: 16, height: 16)
            .animation(.easeInOut(duration: 0.12), value: isWorking)
            .animation(.easeInOut(duration: 0.12), value: session.status)

            if isEditing {
                TextField("", text: $renameDraft)
                    .textFieldStyle(.plain)
                    .font(Theme.sessionLabelFont)
                    .foregroundStyle(Theme.foreground)
                    .multilineTextAlignment(.leading)
                    .focused($renameFocused)
                    .frame(width: renameFieldWidth)
                    .onSubmit(commitRename)
                    .onExitCommand(perform: cancelRename)
                    .onAppear {
                        renameDraft = session.label
                        DispatchQueue.main.async { renameFocused = true }
                    }
                    .onChange(of: renameFocused) { focused in
                        if !focused && isEditing { commitRename() }
                    }
            } else {
                Text(session.label)
                    .font(Theme.sessionLabelFont)
                    // Deliberately quiet — the terminal content is the star;
                    // the active pane's title only reads a touch stronger.
                    .foregroundStyle(
                        isActive
                            ? Theme.mutedForeground
                            : Theme.mutedForeground.opacity(0.65)
                    )
                    .lineLimit(1)
                    .truncationMode(.tail)
                    .simultaneousGesture(
                        TapGesture(count: 2).onEnded { onBeginRename() }
                    )
            }
        }
        .padding(.horizontal, 7)
        .frame(height: 22)
        .background {
            RoundedRectangle(cornerRadius: 7, style: .continuous)
                .fill(hovering ? Theme.hoverRow : .clear)
        }
        .contentShape(RoundedRectangle(cornerRadius: 7, style: .continuous))
        .onHover { hovering = $0 }
        .simultaneousGesture(
            DragGesture(minimumDistance: SidebarSessionDragController.dragThreshold)
                .onChanged { _ in
                    guard !isEditing else { return }
                    if isPanelPane {
                        dragController?.beginPanelPaneDrag(sessionID: session.id)
                    } else {
                        dragController?.beginPaneTitleDrag(
                            sessionID: session.id,
                            groupID: groupID,
                            paneID: paneID
                        )
                    }
                }
                .onEnded { _ in
                    guard !isEditing else { return }
                    if isPanelPane {
                        dragController?.endPanelPaneDrag()
                    } else {
                        dragController?.endPaneTitleDrag()
                    }
                }
        )
        .help(
            isEditing
                ? "Edit session title"
                : "Double-click to rename; drag to move"
        )
    }

    private var renameFieldWidth: CGFloat {
        min(max(CGFloat(max(renameDraft.count, session.label.count)) * 7 + 18, 96), 220)
    }

    private func commitRename() {
        let trimmed = renameDraft.trimmingCharacters(in: .whitespacesAndNewlines)
        if !trimmed.isEmpty && trimmed != session.label {
            onCommitRename(trimmed)
        }
        onEndRename()
    }

    private func cancelRename() {
        onEndRename()
    }
}

/// Supplies a live AppKit window-space frame for each SwiftUI title chip.
/// The view is click-through; SwiftUI owns hover and gesture recognition.
private struct TerminalPaneDragSourceRegistration: NSViewRepresentable {
    let controller: SidebarSessionDragController
    let session: SessionEntry
    let groupID: String?
    let paneID: String

    func makeCoordinator() -> Coordinator { Coordinator() }

    func makeNSView(context: Context) -> SourceView {
        let view = SourceView()
        context.coordinator.update(
            controller: controller,
            view: view,
            session: session,
            groupID: groupID,
            paneID: paneID
        )
        return view
    }

    func updateNSView(_ view: SourceView, context: Context) {
        context.coordinator.update(
            controller: controller,
            view: view,
            session: session,
            groupID: groupID,
            paneID: paneID
        )
    }

    static func dismantleNSView(_ view: SourceView, coordinator: Coordinator) {
        coordinator.remove(view: view)
    }

    final class SourceView: NSView {
        override func hitTest(_: NSPoint) -> NSView? { nil }
    }

    @MainActor
    final class Coordinator {
        private let token = UUID()
        private weak var controller: SidebarSessionDragController?
        private var paneID: String?

        func update(
            controller: SidebarSessionDragController,
            view: NSView,
            session: SessionEntry,
            groupID: String?,
            paneID: String
        ) {
            if let oldController = self.controller,
               let oldPaneID = self.paneID,
               oldController !== controller || oldPaneID != paneID {
                oldController.removePaneSource(paneID: oldPaneID, token: token)
            }
            self.controller = controller
            self.paneID = paneID
            controller.registerPaneSource(
                view: view,
                session: session,
                groupID: groupID,
                paneID: paneID,
                token: token
            )
        }

        func remove(view _: NSView) {
            if let paneID {
                controller?.removePaneSource(paneID: paneID, token: token)
            }
            controller = nil
            paneID = nil
        }
    }
}

// MARK: - Pane card chrome

/// Card treatment for every pane, solo or split: rounded clip, an adaptive
/// foreground hairline that reads slightly stronger on the active pane, and
/// a very subtle drop shadow. The shadow is drawn on a SwiftUI shape behind
/// the card — a Metal-backed terminal never casts one itself — and only in
/// opaque mode: a fill behind a translucent card would double its canvas
/// alpha.
private struct PaneCardChrome: ViewModifier {
    let shape: RoundedRectangle
    let background: NSColor
    let surfaceOpacity: Double
    let isActive: Bool

    func body(content: Content) -> some View {
        content
            .clipShape(shape)
            .overlay {
                shape
                    .strokeBorder(
                        isActive
                            ? Theme.foreground.opacity(0.24)
                            : Theme.contentHairline,
                        lineWidth: 1
                    )
                    .allowsHitTesting(false)
            }
            .background {
                if surfaceOpacity >= 1 {
                    shape
                        .fill(Color(nsColor: background))
                        .shadow(color: .black.opacity(0.07), radius: 2.5, y: 1)
                }
            }
    }
}

// MARK: - Pane container

struct TerminalPaneContainer: View {
    @ObservedObject var store: UnpeelStore
    @ObservedObject private var transparency = TransparencyModel.shared
    @Environment(\.sidebarSessionDragController) private var sessionDragController

    let cache: SurfaceCache
    let representative: SessionEntry
    /// Nil renders the representative as one synthetic full-width pane.
    let group: PaneGroup?
    /// Resolved Session entries for the validated group.
    let entries: [String: SessionEntry]
    /// ContentArea's per-session frame background (OpenCode/Grok theme aware).
    let frameBackground: (SessionEntry) -> NSColor
    /// True for panes hosted OUTSIDE the main content area (the right project
    /// panel). They hide the main-selection-scoped split buttons and skip
    /// split-drop-target registration, since edge-drop splits act on the main
    /// region only.
    var isAuxiliaryRegion: Bool = false

    @State private var activePaneID: String?
    @State private var dividerDrag: DividerDrag?
    @State private var paneConfirmation: PaneConfirmation?
    /// Path key of the split divider currently under the pointer.
    @State private var hoveredDividerID: String?
    /// Pane currently under the pointer — the split buttons show only on the
    /// active pane and the hovered pane, so a multi-pane split isn't cluttered
    /// with a split affordance on every card.
    @State private var hoveredPaneID: String?

    /// Gap between pane cards — the frame-material backdrop shows through,
    /// so every pane reads as its own floating surface. The divider strip IS
    /// the gap (it stays the resize handle).
    // Matches the content-area clip so pane corners land exactly on it.
    private static let paneCornerRadius = Theme.contentCornerRadius

    private struct PresentedPane: Identifiable, Equatable {
        let paneID: String
        let content: PaneContent
        let isSynthetic: Bool

        /// Session identity keeps an existing TerminalHostView mounted while
        /// panes move or a solo Session joins/leaves a group. A launcher
        /// has no Session yet, so its stable pane id is its temporary identity.
        var id: String {
            switch content {
            case let .session(sessionID):
                return "session:\(sessionID)"
            case .launcher:
                return "launcher:\(paneID)"
            }
        }
    }

    /// One divider drag at a time. The live ratio stays local view state;
    /// the model commits once on release, so a drag never changes SwiftUI
    /// structural identity.
    private struct DividerDrag: Equatable {
        let pathKey: String
        let startRatio: CGFloat
        var ratio: CGFloat
    }

    private struct PaneConfirmation: Identifiable {
        enum Action { case archive, remove }
        let action: Action
        let sessionID: String
        let label: String
        let isLive: Bool
        var id: String { "\(sessionID):\(action)" }
    }

    /// Byte-transport choice for the panes in this view. Every workspace on
    /// THIS Mac — this instance's own home or another local workspace
    /// selected through the loopback gateway — renders with the real
    /// `unpeel-attach` surface on its `session.sock`; its home is on this
    /// disk. Only a true remote Host (paired/SSH) uses the paged remote
    /// transport. Sidebar, verbs, and lifecycle for a local workspace still
    /// ride the gateway; this is the terminal data plane only.
    private var usesRemoteTerminalTransport: Bool {
        !store.selectedHostScope.isLocalMachine
    }

    /// Chrome that reads THIS instance's own state (gallery artifacts,
    /// restart recommendations, phone letterbox) stays Local-only even
    /// though another local workspace now shares the attach transport.
    private var isOwnLocalScope: Bool {
        store.selectedHostScope == .local
    }

    private var presentedPanes: [PresentedPane] {
        if let group {
            return group.panes.map { pane in
                PresentedPane(
                    paneID: pane.id,
                    content: pane.content,
                    isSynthetic: false
                )
            }
        }
        return [PresentedPane(
            paneID: "solo:\(representative.id)",
            content: .session(id: representative.id),
            isSynthetic: true
        )]
    }

    /// The zoomed pane, when it belongs to this group and is still present.
    private var zoomedPane: PresentedPane? {
        guard let group,
              let zoomed = store.zoomedTerminalPane,
              zoomed.groupID == group.id
        else { return nil }
        return presentedPanes.first(where: { $0.paneID == zoomed.paneID })
    }

    private var defaultActivePaneID: String {
        if let group {
            return group.panes.first(where: {
                $0.content.sessionID == representative.id
            })?.id ?? group.representativePaneID
        }
        return "solo:\(representative.id)"
    }

    var body: some View {
        Group {
            if let zoomed = zoomedPane {
                // Zoom renders only the zoomed leaf; hidden siblings keep
                // their retained surfaces in the cache.
                paneSlot(zoomed)
                    .id("zoomed:\(zoomed.id)")
            } else if let group {
                subtree(group.root, path: PaneSplitPath())
                    // Ghostty #7546 pattern: structural identity (shape +
                    // leaves, never ratios) tells SwiftUI when the tree
                    // actually changed. Divider drags keep identity stable.
                    .id(group.root.structuralIdentity)
            } else if let solo = presentedPanes.first {
                paneSlot(solo)
            }
        }
        .animation(nil, value: presentedPanes.map(\.id))
        .onAppear {
            syncActivePane()
            claimPendingReveal()
            if let request = store.terminalPaneFocusRequest {
                focusRequestedPane(request)
            }
        }
        .onChange(of: presentedPanes.map(\.paneID)) { _ in
            syncActivePane()
            claimPendingReveal()
        }
        .onChange(of: store.terminalPaneFocusRequest) { request in
            if let request { focusRequestedPane(request) }
        }
        .onReceive(
            NotificationCenter.default.publisher(
                for: .unpeelCloseActivePane,
                object: store
            )
        ) { _ in
            closeActivePane()
        }
        .overlay {
            // In-app confirmation instead of a system alert: on macOS 26 the
            // glass alert over the live Metal surfaces re-rendered its focus
            // ring constantly (and dropped clicks mid-refresh). This overlay
            // is plain SwiftUI the pane already layers chrome with — nothing
            // behind it can make it repaint.
            if let confirmation = paneConfirmation {
                PaneConfirmationOverlay(
                    confirmation: confirmation,
                    onCancel: { paneConfirmation = nil },
                    onConfirm: {
                        paneConfirmation = nil
                        switch confirmation.action {
                        case .archive:
                            store.archiveSession(confirmation.sessionID)
                        case .remove:
                            store.confirmRemoveSession(confirmation.sessionID)
                        }
                    }
                )
            }
        }
    }

    /// Scrim + centered card over the pane area for the archive/remove
    /// confirmation. Esc and click-away cancel (same monitor as the sidebar's
    /// inline confirm row).
    private struct PaneConfirmationOverlay: View {
        let confirmation: PaneConfirmation
        let onCancel: () -> Void
        let onConfirm: () -> Void

        private var title: String {
            switch confirmation.action {
            case .archive:
                return "Stop and archive session?"
            case .remove:
                return confirmation.isLive ? "Remove session?" : "Remove from list?"
            }
        }

        private var message: String {
            switch confirmation.action {
            case .archive:
                return "This stops “\(confirmation.label)” and files it in Unpeel so you can restore and resume later. Screenshots and other session files stay."
            case .remove:
                return "This only removes “\(confirmation.label)” from Unpeel. It does not delete the agent’s conversation."
            }
        }

        private var confirmLabel: String {
            confirmation.action == .archive ? "Archive" : "Remove"
        }

        var body: some View {
            ZStack {
                Color.black.opacity(0.25)
                    .contentShape(Rectangle())
                    .onTapGesture(perform: onCancel)

                VStack(alignment: .leading, spacing: 8) {
                    Text(title)
                        .font(.system(size: 13, weight: .semibold))
                        .foregroundStyle(Theme.foreground)
                    Text(message)
                        .font(.system(size: 12))
                        .foregroundStyle(Theme.mutedForeground)
                        .fixedSize(horizontal: false, vertical: true)
                    HStack(spacing: 8) {
                        Spacer()
                        dialogButton("Cancel", destructive: false, action: onCancel)
                        dialogButton(confirmLabel, destructive: true, action: onConfirm)
                    }
                    .padding(.top, 8)
                }
                .padding(18)
                .frame(width: 320)
                .background(
                    RoundedRectangle(cornerRadius: 14, style: .continuous)
                        .fill(Theme.appBackground)
                        .shadow(color: .black.opacity(0.25), radius: 18, y: 6)
                )
                .overlay(
                    RoundedRectangle(cornerRadius: 14, style: .continuous)
                        .strokeBorder(Theme.contentHairline, lineWidth: 1)
                )
                .background(RemoveConfirmDismissMonitor(onCancel: onCancel))
            }
        }

        private func dialogButton(
            _ label: String, destructive: Bool, action: @escaping () -> Void
        ) -> some View {
            DialogPillButton(
                label: label, destructive: destructive, action: action
            )
        }
    }

    /// Dialog-scale sibling of the sidebar's ConfirmPillButton.
    private struct DialogPillButton: View {
        let label: String
        let destructive: Bool
        let action: () -> Void

        @State private var hovering = false

        var body: some View {
            Button(action: action) {
                Text(label)
                    .font(.system(size: 12, weight: .medium))
                    .foregroundStyle(
                        destructive
                            ? Theme.danger
                            : (hovering ? Theme.foreground : Theme.mutedForeground)
                    )
                    .padding(.horizontal, 12)
                    .frame(height: 24)
                    .background(
                        RoundedRectangle(cornerRadius: 7, style: .continuous)
                            .fill(
                                destructive
                                    ? Theme.danger.opacity(hovering ? 0.25 : 0.15)
                                    : Theme.hoverRow.opacity(hovering ? 1 : 0.6)
                            )
                    )
            }
            .buttonStyle(.plain)
            .onHover { hovering = $0 }
            .animation(.easeInOut(duration: 0.12), value: hovering)
        }
    }

    // MARK: Recursive tree layout

    private static func pathKey(_ path: PaneSplitPath) -> String {
        path.components.map(\.rawValue).joined(separator: ",")
    }

    private func liveRatio(of split: PaneSplit, at path: PaneSplitPath) -> CGFloat {
        if let drag = dividerDrag, drag.pathKey == Self.pathKey(path) {
            return drag.ratio
        }
        return CGFloat(split.ratio)
    }

    /// Recursive walk of the group's tree. AnyView keeps the recursion
    /// type-checkable; trees are at most eight leaves deep, so the erasure
    /// cost is irrelevant next to the Metal surfaces it arranges.
    private func subtree(_ node: PaneNode, path: PaneSplitPath) -> AnyView {
        switch node {
        case let .leaf(pane):
            return AnyView(paneSlot(PresentedPane(
                paneID: pane.id,
                content: pane.content,
                isSynthetic: false
            )))
        case let .split(split):
            return AnyView(splitView(split, path: path))
        }
    }

    private func splitView(_ split: PaneSplit, path: PaneSplitPath) -> some View {
        GeometryReader { geo in
            let ratio = liveRatio(of: split, at: path)
            let horizontal = split.direction == .horizontal
            let extent = max(
                0,
                (horizontal ? geo.size.width : geo.size.height)
                    - TerminalPaneDropPreviewGeometry.dividerWidth
            )
            let first = extent * ratio
            let second = extent - first
            let leftPath = PaneSplitPath(path.components + [.left])
            let rightPath = PaneSplitPath(path.components + [.right])
            if horizontal {
                HStack(spacing: 0) {
                    subtree(split.left, path: leftPath)
                        .frame(width: first)
                    divider(split: split, path: path, extent: extent)
                    subtree(split.right, path: rightPath)
                        .frame(width: second)
                }
            } else {
                VStack(spacing: 0) {
                    subtree(split.left, path: leftPath)
                        .frame(height: first)
                    divider(split: split, path: path, extent: extent)
                    subtree(split.right, path: rightPath)
                        .frame(height: second)
                }
            }
        }
    }

    // MARK: Pane content

    @ViewBuilder
    private func paneSlot(_ pane: PresentedPane) -> some View {
        switch pane.content {
        case let .session(sessionID):
            if let entry = entry(for: sessionID) {
                sessionPane(pane, entry: entry)
            } else {
                unavailablePane(pane)
            }
        case let .launcher(projectID):
            launcherPane(pane, projectID: projectID)
        }
    }

    private func entry(for sessionID: String) -> SessionEntry? {
        entries[sessionID] ?? (sessionID == representative.id ? representative : nil)
    }

    /// One terminal pane. The enclosing split (or the container, for solo and
    /// zoomed panes) provides final geometry; the pane fills it.
    private func sessionPane(
        _ pane: PresentedPane,
        entry: SessionEntry
    ) -> some View {
        let background = frameBackground(entry)
        return VStack(spacing: 0) {
            // With the window titlebar gone the header is the pane's only
            // chrome — always visible, solo terminals included.
            paneHeader(pane: pane, entry: entry, title: entry.label, background: background)

            // Session banners are pane-scoped: in a split only the affected
            // pane shows them, painted on its own surface color.
            paneBanners(for: entry, background: background)

            ZStack {
                // Under-surface backstop: with a translucent terminal the
                // surface alone paints the canvas (matching SwapContainer).
                Color(nsColor: background)
                    .opacity(transparency.surfaceOpacity < 1 ? 0 : 1)
                Group {
                    if usesRemoteTerminalTransport {
                        RemoteScopeTerminalMount(
                            store: store,
                            session: entry,
                            backgroundColor: background,
                            isActive: effectiveActivePaneID == pane.paneID,
                            onActivate: { activatePane(pane.paneID) }
                        )
                    } else {
                        TerminalHostView(
                            session: entry,
                            workingDirectory: store.paneWorkingDirectory(for: entry),
                            sessionsDir: store.scopedSessionsDir,
                            frameBackgroundColor: background,
                            themeRevision: cache.themeRevision,
                            phoneResize: isOwnLocalScope
                                ? store.phoneResizeOverrides[entry.id]
                                : nil,
                            cache: cache,
                            isActive: effectiveActivePaneID == pane.paneID,
                            onActivate: { activatePane(pane.paneID) },
                            onCommandClick: { match, path in
                                store.openClickedFile(
                                    match,
                                    path: path,
                                    fromSessionID: entry.id
                                )
                            }
                        )
                    }
                }
            }
            .frame(maxHeight: .infinity)
            .overlay(alignment: .topTrailing) {
                paneCornerControls(for: entry)
            }
            .overlay {
                if let approval = store.pendingMcpApproval(forSessionID: entry.id) {
                    McpApprovalPaneOverlay(
                        store: store,
                        approval: approval,
                        capturesKeys: effectiveActivePaneID == pane.paneID
                    )
                }
            }
        }
        .modifier(paneCardChrome(background: background, paneID: pane.paneID))
        .onHover { hovering in
            if hovering {
                hoveredPaneID = pane.paneID
            } else if hoveredPaneID == pane.paneID {
                hoveredPaneID = nil
            }
        }
        .overlay { paneDropHighlight(for: pane) }
        .overlay { paneSortPreviewHighlight(for: pane) }
        .background {
            if !isAuxiliaryRegion, let sessionDragController {
                TerminalPaneDropTargetRegistration(
                    controller: sessionDragController,
                    paneID: pane.paneID,
                    isSolo: pane.isSynthetic
                )
            } else if isAuxiliaryRegion, let sessionDragController,
                      case let .session(sessionID) = pane.content {
                // Panel panes register their full card frame: the drag's
                // source geometry AND the hover target for stack reordering.
                PanelPaneDragSourceRegistration(
                    controller: sessionDragController,
                    sessionID: sessionID
                )
            }
        }
        .transition(.identity)
    }

    /// Half-pane highlight while a dragged Session hovers one of this pane's
    /// four edges: dropping would split this pane there.
    @ViewBuilder
    private func paneDropHighlight(for pane: PresentedPane) -> some View {
        if case let .pane(paneID, edge) = store.terminalPaneDropTarget,
           paneID == pane.paneID {
            GeometryReader { geo in
                let shape = RoundedRectangle(cornerRadius: 10, style: .continuous)
                shape
                    .fill(Theme.hoverRow)
                    .overlay(
                        shape.stroke(Theme.foreground.opacity(0.14), lineWidth: 1)
                    )
                    .frame(
                        width: edge == .left || edge == .right
                            ? geo.size.width / 2 - 6
                            : geo.size.width - 12,
                        height: edge == .up || edge == .down
                            ? geo.size.height / 2 - 6
                            : geo.size.height - 12
                    )
                    .padding(6)
                    .frame(
                        maxWidth: .infinity,
                        maxHeight: .infinity,
                        alignment: {
                            switch edge {
                            case .left: return .leading
                            case .right: return .trailing
                            case .up: return .top
                            case .down: return .bottom
                            }
                        }()
                    )
            }
            .allowsHitTesting(false)
            .transition(.opacity)
            .animation(
                .easeOut(duration: 0.12),
                value: store.terminalPaneDropTarget
            )
        }
    }
    /// Sort/swap preview: the hovered target pane carries a full-card
    /// highlight (the same visual language as the edge-split drop zones), and
    /// the actual swap/reorder commits only on drop. A NON-member pin drop on
    /// a panel pane previews as the half where the arriving pane lands.
    @ViewBuilder
    private func paneSortPreviewHighlight(for pane: PresentedPane) -> some View {
        let shape = RoundedRectangle(cornerRadius: 10, style: .continuous)
        let band = shape
            .fill(Theme.hoverRow)
            .overlay(
                shape.stroke(Theme.foreground.opacity(0.14), lineWidth: 1)
            )
        if isAuxiliaryRegion,
           let sessionID = pane.content.sessionID,
           let insert = store.projectSidebarInsertPreview,
           insert.sessionID == sessionID {
            GeometryReader { geo in
                band
                    .frame(height: geo.size.height / 2 - 6)
                    .padding(6)
                    .frame(
                        maxHeight: .infinity,
                        alignment: insert.below ? .bottom : .top
                    )
            }
            .allowsHitTesting(false)
            .transition(.opacity)
        } else if isAuxiliaryRegion
            ? pane.content.sessionID != nil
                && pane.content.sessionID == store.projectSidebarReorderPreviewID
            : pane.paneID == store.paneSwapPreviewPaneID {
            band
                .padding(6)
                .allowsHitTesting(false)
                .transition(.opacity)
        }
    }

    private var paneCardShape: RoundedRectangle {
        RoundedRectangle(
            cornerRadius: Self.paneCornerRadius,
            style: .continuous
        )
    }

    /// Card chrome for a pane at its final width: clips to the card shape
    /// and adds a VERY subtle drop shadow into the frame-material gaps.
    private func paneCardChrome(
        background: NSColor,
        paneID: String
    ) -> some ViewModifier {
        PaneCardChrome(
            shape: paneCardShape,
            background: background,
            surfaceOpacity: transparency.surfaceOpacity,
            isActive: paneShowsActiveBorder(paneID: paneID)
        )
    }

    /// The ONE globally focused pane. A multi-pane split uses its own active
    /// pane. A solo / project-sidebar pane is focused only when it is the last
    /// solo pane clicked (defaulting to the main area) and focus is not held by
    /// a split group. This is what makes "focus" meaningful ACROSS the main
    /// area and the project sidebar, where each pane is its own solo container.
    private func paneIsFocused(_ pane: PresentedPane) -> Bool {
        if presentedPanes.count > 1 {
            return effectiveActivePaneID == pane.paneID
        }
        guard store.activeTerminalPane == nil else { return false }
        if let focused = store.focusedSoloSessionID {
            return pane.content.sessionID == focused
        }
        return !isAuxiliaryRegion
    }

    /// Whether this pane wears the white active-focus border. Only meaningful
    /// when the window actually holds more than one pane — a split, or the
    /// project sidebar contributing panes beside the main one — and then it
    /// follows the focused pane.
    private func paneShowsActiveBorder(paneID: String) -> Bool {
        guard zoomedPane == nil,
              let pane = presentedPanes.first(where: { $0.paneID == paneID })
        else { return false }
        let multiPane = presentedPanes.count > 1 || !store.projectSidebarSessions.isEmpty
        return multiPane && paneIsFocused(pane)
    }

    /// Empty pane shown while the user chooses a preset. It is transient and
    /// never appears in DurablePaneLayout.
    private func launcherPane(
        _ pane: PresentedPane,
        projectID: String
    ) -> some View {
        let project = store.projectsByID[projectID]
            ?? store.displayProjectsByID[projectID]
        let background = frameBackground(representative)
        let starting = store.paneLaunchIsPending(pane.paneID)
        return VStack(spacing: 0) {
            paneHeader(
                pane: pane,
                entry: nil,
                title: starting ? "Starting…" : "New pane",
                background: background
            )
            ZStack {
                Color(nsColor: background)
                    .opacity(transparency.surfaceOpacity)
                if starting {
                    BrailleSpinner(color: Theme.mutedForeground)
                } else if let project {
                    SessionLauncherView(
                        store: store,
                        project: project,
                        compact: true,
                        onLaunch: { preset in launch(preset, into: pane) },
                        onCancel: { detachPane(pane) }
                    )
                }
            }
            .frame(maxHeight: .infinity)
        }
        .modifier(paneCardChrome(background: background, paneID: pane.paneID))
        .transition(.identity)
    }

    /// Defensive render-time fallback. A validated group normally supplies
    /// every Session entry, but retaining the slot avoids a geometry jump if
    /// a projection changes during the current render pass.
    private func unavailablePane(_ pane: PresentedPane) -> some View {
        let background = frameBackground(representative)
        return VStack(spacing: 0) {
            paneHeader(
                pane: pane,
                entry: nil,
                title: "Session unavailable",
                background: background
            )
            ZStack {
                Color(nsColor: background)
                    .opacity(transparency.surfaceOpacity)
                Text("Session unavailable")
                    .font(Theme.sessionLabelFont)
                    .foregroundStyle(Theme.mutedForeground)
            }
            .frame(maxHeight: .infinity)
        }
        .modifier(paneCardChrome(background: background, paneID: pane.paneID))
        .transition(.identity)
    }

    /// The title sits after the reserved spinner slot so busy/idle does not
    /// shift it; an idle session shows its runtime logo in that slot, and the
    /// more-menu stays on the trailing edge.
    private func paneHeader(
        pane: PresentedPane,
        entry: SessionEntry?,
        title: String,
        background: NSColor
    ) -> some View {
        ZStack {
            // Chrome strip: matches the pane's translucent canvas alpha.
            // It doubles as a WINDOW drag surface — with the titlebar gone
            // the header is the top chrome, so grabbing any empty spot moves
            // the window. The gesture lives on this background layer only:
            // the chip, gallery, split, and more-menu sit above and claim
            // their own clicks first, so a pane-title drag never fights it.
            Color(nsColor: background)
                .opacity(transparency.surfaceOpacity)
                .contentShape(Rectangle())
                .gesture(
                    DragGesture(minimumDistance: 3)
                        .onChanged { _ in
                            guard let event = NSApp.currentEvent,
                                  event.type == .leftMouseDragged,
                                  let window = event.window
                            else { return }
                            window.performDrag(with: event)
                        }
                )
            // auto / flex-1: the title chip is left-aligned and the
            // trailing controls take the remaining width, staying pinned
            // to the pane's right edge.
            HStack(spacing: 0) {
                if isAuxiliaryRegion, entry != nil {
                    // Panel membership mark: these panes are "pinned to the
                    // project sidebar" (the context-menu verb's counterpart).
                    ChromeIconView(icon: .pushPin, size: 11)
                        .foregroundStyle(Theme.mutedForeground.opacity(0.7))
                        .frame(width: 14, height: 14)
                        .padding(.leading, 4)
                        .allowsHitTesting(false)
                }
                if let entry {
                    TerminalPaneTitleChip(
                        session: entry,
                        groupID: group?.id,
                        paneID: pane.paneID,
                        isActive: effectiveActivePaneID == pane.paneID,
                        isEditing: store.editingSessionID == entry.id
                            && store.editingSessionSurface == .paneHeader,
                        isWorking: paneIsWorking(entry),
                        tint: paneSpinnerColor(entry),
                        dragController: sessionDragController,
                        isPanelPane: isAuxiliaryRegion,
                        onBeginRename: {
                            store.beginEditingSessionTitle(entry.id, on: .paneHeader)
                        },
                        onCommitRename: { store.renameSession(entry.id, to: $0) },
                        onEndRename: {
                            if store.editingSessionID == entry.id {
                                store.editingSessionID = nil
                            }
                        }
                    )
                } else {
                    Text(title)
                        .font(Theme.sessionLabelFont)
                        .foregroundStyle(Theme.mutedForeground)
                        .lineLimit(1)
                        .truncationMode(.tail)
                        .padding(.horizontal, 7)
                }

                HStack(spacing: 0) {
                    Spacer(minLength: 0)

                    // Same verbs as Session ▸ Split Pane Right (⌘D) / Down
                    // (⌘⇧D): empty launcher splitting THIS pane. Hidden when
                    // those menu items would be disabled (remote, full group,
                    // launcher already open) and shown only on the active or
                    // hovered pane so a multi-pane split stays uncluttered.
                    if showsPaneSplitControls(for: pane) {
                        if isAuxiliaryRegion {
                            // The panel stacks vertically: one split verb, the
                            // same launcher ⌘D opens, arriving right below THIS
                            // pane (launcher-at-destination, like main splits).
                            PaneSplitButton(
                                icon: .splitPaneDown,
                                help: "Add Pane (⌘D)",
                                label: "Add Pane"
                            ) {
                                store.openProjectSidebarLauncherFromPanel(
                                    afterSessionID: entry?.id
                                )
                            }
                            .padding(.trailing, 2)
                        } else if store.canOpenPaneLauncher() {
                            PaneSplitButton(
                                icon: .splitPane,
                                help: "Split Pane Right (⌘D)",
                                label: "Split Pane Right"
                            ) {
                                store.openPaneLauncher(
                                    at: .right,
                                    splitting: pane.isSynthetic ? nil : pane.paneID
                                )
                            }
                            PaneSplitButton(
                                icon: .splitPaneDown,
                                help: "Split Pane Down (⌘⇧D)",
                                label: "Split Pane Down"
                            ) {
                                store.openPaneLauncher(
                                    at: .down,
                                    splitting: pane.isSynthetic ? nil : pane.paneID
                                )
                            }
                            .padding(.trailing, 2)
                        }
                    }

                    paneMoreMenu(for: pane, entry: entry)
                }
                .frame(maxWidth: .infinity)
            }
            .padding(.horizontal, 8)
            .padding(.top, 3.5)
            .padding(.bottom, 2)
        }
        .frame(height: Theme.sessionRowHeight + 5.5)
        .background {
            // The chip is the drag handle; the whole header is the sort target.
            if let entry, let sessionDragController {
                TerminalPaneDragSourceRegistration(
                    controller: sessionDragController,
                    session: entry,
                    groupID: group?.id,
                    paneID: pane.paneID
                )
            }
        }
    }

    /// The per-session notice bars (restart recommendation, resume failure,
    /// phone resize), scoped to THIS pane's session and painted on its
    /// terminal surface color (matching the pane's translucency). Local
    /// scope only — remote sessions surface connection state through the
    /// area-level banner instead.
    @ViewBuilder
    private func paneBanners(for entry: SessionEntry, background: NSColor) -> some View {
        if isOwnLocalScope {
            let surface = Color(nsColor: background)
                .opacity(transparency.surfaceOpacity)
            if let recommendation = store.restartRecommendations[entry.id] {
                RestartRecommendedBar(
                    recommendation: recommendation,
                    background: surface,
                    onRestart: {
                        switch recommendation.action {
                        case .resumeAgent:
                            store.resumeAgent(entry.id)
                        case .reloadTerminal:
                            store.restartSession(entry.id, stoppedOnly: false)
                        case nil:
                            break
                        }
                    },
                    onDismiss: { store.dismissRestartRecommendation(for: entry.id) }
                )
            }
            if store.resumeFailures.contains(entry.id) {
                ResumeFailedBar(
                    background: surface,
                    onStartFresh: { store.startFreshAfterResumeFailure(entry.id) },
                    onDismiss: { store.dismissResumeFailure(for: entry.id) }
                )
            }
        }
    }

    // MARK: Corner controls

    /// A pane whose Session hosts a recognized agent or App occupant, or was
    /// launched as one — never a plain shell. Only these panes get the
    /// top-right corner controls.
    private func isAgentTerminal(_ entry: SessionEntry) -> Bool {
        entry.activeRuntimeID != nil
            || entry.activeApp != nil
            || SetupTool.detect(in: entry.command) != nil
    }

    /// Ghost controls in the terminal's top-right corner, stacked
    /// left-to-right [fit to desktop][gallery]: the return-to-desktop-grid
    /// affordance while a phone drives this session's size, and the
    /// gallery/screenshot chip. They sit on the terminal surface below the
    /// pane header, so they never collide with the header's split/more
    /// glyphs or the title strip's activity button. Empty (and outside the
    /// terminal's hit area) whenever nothing applies.
    @ViewBuilder
    private func paneCornerControls(for entry: SessionEntry) -> some View {
        let phoneResize = isOwnLocalScope ? store.phoneResizeOverrides[entry.id] : nil
        let showsGallery = isOwnLocalScope && store.showSessionGallery
        if isAgentTerminal(entry), phoneResize != nil || showsGallery {
            HStack(spacing: 2) {
                if let phoneResize {
                    PaneFitToDesktopButton(grid: phoneResize) {
                        store.clearPhoneResizeOverride(for: entry.id)
                    }
                }
                if showsGallery {
                    SessionGalleryButton(sessionID: entry.id, cache: cache, ghost: true)
                }
            }
            .padding(.top, 4)
            .padding(.trailing, 6)
        }
    }

    // MARK: Membership

    /// Ghostty-style ⌘W applies to the focused leaf, including the synthetic
    /// full-size leaf of a solo terminal. A resumable agent gets the same
    /// non-system confirmation card as the pane menu; confirming archives
    /// rather than removing the Unpeel session.
    private func closeActivePane() {
        guard let pane = presentedPanes.first(where: {
            $0.paneID == effectiveActivePaneID
        }) else { return }

        let sessionID = pane.content.sessionID
        let action = terminalPaneCloseAction(
            for: pane.content,
            canArchiveSession: sessionID.map(store.sessionCanArchive) ?? false
        )
        switch action {
        case .detachPane:
            detachPane(pane)
        case let .removeSession(sessionID):
            // A stale defensive slot must never turn ⌘W into an unknown-id
            // Host mutation. Real mounted Session panes always resolve here.
            guard entry(for: sessionID) != nil else {
                detachPane(pane)
                return
            }
            store.confirmRemoveSession(sessionID)
        case let .confirmArchive(sessionID):
            guard let entry = entry(for: sessionID) else {
                detachPane(pane)
                return
            }
            paneConfirmation = PaneConfirmation(
                action: .archive,
                sessionID: sessionID,
                label: entry.label,
                isLive: entry.isLive
            )
        }
    }

    private func detachPane(_ pane: PresentedPane) {
        guard !pane.isSynthetic else { return }
        store.detachTerminalPane(pane.paneID)
    }

    private func launch(_ preset: Preset, into pane: PresentedPane) {
        guard !pane.isSynthetic else { return }
        store.launchSessionIntoPane(
            representativeSessionID: representative.id,
            launcherPaneID: pane.paneID,
            preset: preset
        )
    }

    // MARK: Focus

    private var effectiveActivePaneID: String {
        if let activePaneID,
           presentedPanes.contains(where: { $0.paneID == activePaneID }) {
            return activePaneID
        }
        if let group,
           let active = store.activeTerminalPane,
           active.groupID == group.id,
           presentedPanes.contains(where: { $0.paneID == active.paneID }) {
            return active.paneID
        }
        return defaultActivePaneID
    }

    /// Split affordances show only on the focused pane (the one wearing the
    /// white border) and the pane under the pointer — never on every card.
    /// `effectiveActivePaneID` alone is useless here: a solo container (the
    /// lone main pane, and every project-sidebar pane) always reports itself
    /// active, which is exactly what made the buttons show everywhere. The
    /// border logic already resolves the ONE globally focused pane.
    private func showsPaneSplitControls(for pane: PresentedPane) -> Bool {
        hoveredPaneID == pane.paneID || paneIsFocused(pane)
    }

    private func activatePane(_ paneID: String) {
        guard presentedPanes.contains(where: { $0.paneID == paneID }) else { return }
        if activePaneID != paneID {
            activePaneID = paneID
        }
        guard let group else {
            store.clearActiveTerminalPane()
            // Solo/panel pane: remember it as the focused pane so the white
            // border can follow focus across the main area and project sidebar.
            store.setFocusedSoloSession(
                presentedPanes.first(where: { $0.paneID == paneID })?.content.sessionID
            )
            return
        }
        let sessionID = presentedPanes.first(where: { $0.paneID == paneID })?
            .content.sessionID
        store.setActiveTerminalPane(
            groupID: group.id,
            paneID: paneID,
            sessionID: sessionID
        )
    }

    /// Restore this window's transient active pane when the container remounts
    /// (for example after Settings), or fall back to the representative when
    /// the previous active pane left the group.
    private func syncActivePane() {
        guard let group else {
            activePaneID = defaultActivePaneID
            store.clearActiveTerminalPane()
            return
        }
        if let active = store.activeTerminalPane,
           active.groupID == group.id,
           group.panes.contains(where: { $0.id == active.paneID }) {
            activePaneID = active.paneID
            let sessionID = group.panes.first(where: {
                $0.id == active.paneID
            })?.content.sessionID
            store.setActiveTerminalPane(
                groupID: group.id,
                paneID: active.paneID,
                sessionID: sessionID
            )
            return
        }
        activatePane(defaultActivePaneID)
    }

    private func paneIsWorking(_ entry: SessionEntry) -> Bool {
        entry.status == .starting
            || entry.status == .busy
            || store.restartingSessionIDs.contains(entry.id)
            || store.resumingAgentSessionIDs.contains(entry.id)
    }

    private func paneSpinnerColor(_ entry: SessionEntry) -> Color {
        store.remoteSpinnerColor(for: entry.id)
            ?? Theme.toolSpinnerColor(forCommand: entry.presentationCommand)
    }

    /// Focus the exact terminal requested by a sidebar pane tile. Activation
    /// is established before the AppKit surface exists: local and remote
    /// mounts both focus themselves when their `isActive` input becomes true,
    /// so a slow cold mount cannot lose the user's intent.
    private func focusRequestedPane(_ request: PaneLayoutController.FocusRequest) {
        guard let group,
              request.groupID == group.id,
              group.representativeSessionID == store.selectedSessionID,
              group.panes.contains(where: { $0.id == request.paneID }),
              store.terminalPaneFocusRequest == request
        else { return }
        activatePane(request.paneID)
        guard store.consumeTerminalPaneFocus(request) else { return }

        if usesRemoteTerminalTransport {
            _ = store.remoteHostRuntime.focusTerminalPane(request.sessionID)
        } else if let terminal = cache.existingPane(for: request.sessionID) {
            terminal.focus()
            terminal.renderNow()
        }
    }

    /// Claim the one-shot insertion marker and focus the inserted pane.
    /// Geometry is already final; new content appears in place with no
    /// entrance motion (a content scale over the Metal surface read as a
    /// stuck padding inset when the reveal dance misfired).
    private func claimPendingReveal() {
        guard let group,
              let pending = store.pendingPaneReveal(in: group.id),
              presentedPanes.contains(where: { $0.paneID == pending })
        else { return }
        _ = store.consumePaneReveal(groupID: group.id, paneID: pending)
        activatePane(pending)
    }

    // MARK: Pane menus

    private func paneMoreMenu(
        for pane: PresentedPane,
        entry: SessionEntry?
    ) -> some View {
        PaneMoreMenuButton(makeMenu: { controller in
            paneMenu(for: pane, entry: entry, controller: controller)
        }) {
            paneControlGlyph("ellipsis")
        }
        .frame(width: 26, height: Theme.sessionRowHeight)
        .contentShape(Rectangle())
        .help("More")
    }

    /// Native menu twin of the former SwiftUI `Menu`. SwiftUI always anchors
    /// a macOS menu's leading edge to its label, which made this trailing-edge
    /// control open down-right. The controller below positions the same rows
    /// with the popup's trailing edge aligned to the ellipsis instead.
    private func paneMenu(
        for pane: PresentedPane,
        entry: SessionEntry?,
        controller: PaneMoreMenuController
    ) -> NSMenu {
        let menu = controller.makeMenu()

        if let entry {
            if entry.supportsTranscriptCopy {
                let transcriptMenu = controller.makeMenu()
                transcriptMenu.addItem(controller.item("Last 20 entries") {
                    store.copyTranscriptMarkdown(entry.id, entries: 20)
                })
                transcriptMenu.addItem(controller.item("Last 50 entries") {
                    store.copyTranscriptMarkdown(entry.id, entries: 50)
                })
                transcriptMenu.addItem(controller.item("Whole conversation") {
                    store.copyTranscriptMarkdown(entry.id, entries: 0)
                })
                let transcriptItem = NSMenuItem(
                    title: "Copy transcript",
                    action: nil,
                    keyEquivalent: ""
                )
                transcriptItem.submenu = transcriptMenu
                menu.addItem(transcriptItem)
            }

            menu.addItem(controller.item("Copy session ID") {
                NSPasteboard.general.clearContents()
                NSPasteboard.general.setString(
                    "Unpeel Session ID: \(entry.id)", forType: .string
                )
            })

            // The panel's pin/un-pin verbs live HERE on desktop: the Sidebar
            // group row is hidden from the desktop tree, so the pane menu is
            // the member's own surface. A main-area pane offers the way IN
            // (detaching from its split first); a panel pane the way out.
            if isAuxiliaryRegion, store.sessionIsInProjectSidebar(entry.id) {
                menu.addItem(.separator())
                menu.addItem(controller.item("Unpin from global project sidebar") {
                    store.moveSessionToMainArea(entry.id)
                })
            } else if !isAuxiliaryRegion,
                      store.canMoveSessionToProjectSidebar(entry.id) {
                menu.addItem(.separator())
                menu.addItem(controller.item("Pin to global project sidebar") {
                    store.moveSessionToProjectSidebar(entry.id)
                })
            }

            menu.addItem(.separator())
            addPaneSessionItems(to: menu, entry: entry, controller: controller)
        } else if case .launcher = pane.content {
            menu.addItem(presetMenuItem(
                .newTerminal,
                controller: controller,
                action: { launch(.newTerminal, into: pane) }
            ))
            if !store.displayAvailablePresets.isEmpty {
                menu.addItem(.separator())
                for preset in store.displayAvailablePresets {
                    menu.addItem(presetMenuItem(
                        preset,
                        controller: controller,
                        action: { launch(preset, into: pane) }
                    ))
                }
            }
            menu.addItem(.separator())
            menu.addItem(controller.item("Manage Agents & Apps…") {
                store.openSettings(tab: .presets)
            })
        }

        if let group {
            if !menu.items.isEmpty { menu.addItem(.separator()) }
            menu.addItem(controller.item("Detach Pane") { detachPane(pane) })
            menu.addItem(controller.item("Exit Multi-Pane View") {
                store.closeTerminalPaneGroup(group.id)
            })
        }

        return menu
    }

    private func addPaneSessionItems(
        to menu: NSMenu,
        entry: SessionEntry,
        controller: PaneMoreMenuController
    ) {
        if store.sessionCanResumeAgent(entry.id) {
            menu.addItem(controller.item("Resume Agent") {
                store.resumeAgentOrSession(entry.id)
            })
        } else if store.sessionCanRestart(entry.id) {
            menu.addItem(controller.item("Resume") {
                store.resumeAgentOrSession(entry.id)
            })
        }

        if store.sessionCanNotifyWhenDone(entry.id) {
            let enabled = store.notifyWhenDoneSessionIDs.contains(entry.id)
            menu.addItem(controller.item(
                "Notify when done",
                state: enabled ? .on : .off
            ) {
                store.setNotifyWhenDone(entry.id, enabled: !enabled)
            })
        }

        if entry.status == .attention,
           store.sessionCanClearAttention(entry.id) {
            menu.addItem(controller.item("Clear attention") {
                store.clearAttention(entry.id)
            })
        }

        menu.addItem(.separator())

        if store.sessionCanArchive(entry.id) {
            menu.addItem(controller.item(
                entry.isLive ? "Stop and archive" : "Archive"
            ) {
                requestPaneArchive(entry)
            })
        }
        menu.addItem(controller.item(
            entry.isLive ? "Remove session" : "Remove from list"
        ) {
            // For a non-resumable launch this is the pane's counterpart to
            // the sidebar X: there is nothing to archive, so remove it in
            // one click even while its terminal is still live. Resumable
            // Sessions keep the destructive confirmation.
            guard store.sessionCanArchive(entry.id) else {
                store.confirmRemoveSession(entry.id)
                return
            }
            paneConfirmation = PaneConfirmation(
                action: .remove,
                sessionID: entry.id,
                label: entry.label,
                isLive: entry.isLive
            )
        })
    }

    private func presetMenuItem(
        _ preset: Preset,
        controller: PaneMoreMenuController,
        action: @escaping () -> Void
    ) -> NSMenuItem {
        let item = controller.item(preset.label, action: action)
        let runtime = UnpeelRuntimeCatalog.runtime(command: preset.command)
        let icon = runtime.map(UnpeelToolIcon.forRuntime) ?? .terminal
        if let image = ToolIcons.image(for: icon)?.copy() as? NSImage {
            image.size = NSSize(width: 16, height: 16)
            item.image = image
        }
        return item
    }

    private func requestPaneArchive(_ entry: SessionEntry) {
        switch entry.status {
        case .starting, .busy, .attention:
            paneConfirmation = PaneConfirmation(
                action: .archive,
                sessionID: entry.id,
                label: entry.label,
                isLive: entry.isLive
            )
        case .idle, .exited:
            store.requestArchiveSession(entry.id)
        }
    }

    private func paneControlGlyph(_ systemName: String) -> some View {
        Image(systemName: systemName)
            .font(.system(size: 9, weight: .semibold))
            .foregroundStyle(Theme.foreground.opacity(0.75))
            .frame(width: 26, height: Theme.sessionRowHeight)
            .contentShape(Rectangle())
    }

    // MARK: Divider

    /// The gap between a split's two children doubles as the resize grab
    /// strip. It paints nothing — the frame-material backdrop shows through
    /// — except a slim centered grip on hover so the affordance still reads.
    /// Moving it changes only this split's ratio; every other split is fixed.
    /// The live drag renders from local state; the model commits on release.
    private func divider(
        split: PaneSplit,
        path: PaneSplitPath,
        extent: CGFloat
    ) -> some View {
        let key = Self.pathKey(path)
        let horizontal = split.direction == .horizontal
        return ZStack {
            if hoveredDividerID == key {
                Capsule()
                    .fill(Theme.paneDividerLineHover)
                    .frame(
                        width: horizontal ? 3 : 36,
                        height: horizontal ? 36 : 3
                    )
                    .transition(.opacity)
            }
        }
        .frame(
            width: horizontal ? TerminalPaneDropPreviewGeometry.dividerWidth : nil,
            height: horizontal ? nil : TerminalPaneDropPreviewGeometry.dividerWidth
        )
        .frame(
            maxWidth: horizontal
                ? TerminalPaneDropPreviewGeometry.dividerWidth
                : .infinity,
            maxHeight: horizontal
                ? .infinity
                : TerminalPaneDropPreviewGeometry.dividerWidth
        )
        .contentShape(Rectangle())
        .transition(.identity)
        .animation(
            .easeInOut(duration: 0.12),
            value: hoveredDividerID == key
        )
        .onHover { hovering in
            if hovering {
                (horizontal ? NSCursor.resizeLeftRight : NSCursor.resizeUpDown).set()
                hoveredDividerID = key
            } else {
                NSCursor.arrow.set()
                if hoveredDividerID == key {
                    hoveredDividerID = nil
                }
            }
        }
        .gesture(
            DragGesture(minimumDistance: 1, coordinateSpace: .global)
                .onChanged { value in
                    guard group != nil, extent > 0 else { return }
                    let start = dividerDrag?.pathKey == key
                        ? dividerDrag!.startRatio
                        : CGFloat(split.ratio)
                    let translation = horizontal
                        ? value.translation.width
                        : value.translation.height
                    let requested = start + translation / extent
                    let applied = min(
                        max(requested, PaneLayoutState.minimumSplitRatio),
                        PaneLayoutState.maximumSplitRatio
                    )
                    dividerDrag = DividerDrag(
                        pathKey: key,
                        startRatio: start,
                        ratio: applied
                    )
                }
                .onEnded { _ in
                    if let drag = dividerDrag, drag.pathKey == key, let group {
                        store.resizePaneSplit(
                            groupID: group.id,
                            path: path,
                            ratio: Double(drag.ratio)
                        )
                    }
                    dividerDrag = nil
                }
        )
    }
}

/// Ellipsis control backed by a hand-positioned AppKit menu. Its invisible
/// anchor tracks the exact button frame, so the menu opens immediately below
/// and extends left even when this pane is flush with the window's right edge.
@MainActor
private struct PaneMoreMenuButton<Label: View>: View {
    let makeMenu: (PaneMoreMenuController) -> NSMenu
    @ViewBuilder let label: () -> Label

    @State private var controller = PaneMoreMenuController()

    var body: some View {
        Button {
            controller.present(makeMenu(controller))
        } label: {
            label()
        }
        .buttonStyle(.plain)
        .compatFocusEffectDisabled()
        .background(PaneMoreMenuAnchor(controller: controller))
        .accessibilityLabel("More")
    }
}

/// Owns menu action closures for the duration of AppKit's modal menu loop and
/// right-aligns the popup to its anchor. `popUp` returns after dismissal, so
/// dropping the closure table there cannot race a selected action.
@MainActor
private final class PaneMoreMenuController: NSObject {
    weak var anchorView: NSView?
    private var actions: [String: () -> Void] = [:]

    func makeMenu() -> NSMenu {
        let menu = NSMenu()
        menu.autoenablesItems = false
        return menu
    }

    func item(
        _ title: String,
        state: NSControl.StateValue = .off,
        action: @escaping () -> Void
    ) -> NSMenuItem {
        let actionID = UUID().uuidString
        actions[actionID] = action
        let item = NSMenuItem(
            title: title,
            action: #selector(handleSelection(_:)),
            keyEquivalent: ""
        )
        item.target = self
        item.representedObject = actionID
        item.state = state
        return item
    }

    func present(_ menu: NSMenu) {
        guard let anchorView else {
            actions.removeAll()
            return
        }
        let origin = NSPoint(
            x: anchorView.bounds.maxX - menu.size.width,
            y: -4
        )
        menu.popUp(positioning: nil, at: origin, in: anchorView)
        actions.removeAll()
    }

    @objc private func handleSelection(_ sender: NSMenuItem) {
        guard let actionID = sender.representedObject as? String else { return }
        actions[actionID]?()
    }
}

/// Invisible AppKit view tracking the ellipsis frame for native menu
/// placement (the same pattern used by WorkspaceOpenMenu and gallery menus).
@MainActor
private struct PaneMoreMenuAnchor: NSViewRepresentable {
    let controller: PaneMoreMenuController

    func makeNSView(context: Context) -> NSView {
        let view = NSView()
        controller.anchorView = view
        return view
    }

    func updateNSView(_ nsView: NSView, context: Context) {
        controller.anchorView = nsView
    }
}

/// Pane-header Split Pane control — one per direction, same actions as
/// Session ▸ Split Pane Right (⌘D) / Split Pane Down (⌘⇧D).
private struct PaneSplitButton: View {
    let icon: ChromeIcon
    let help: String
    let label: String
    let action: () -> Void

    @State private var hovering = false

    var body: some View {
        Button(action: action) {
            ChromeIconView(icon: icon, size: 14)
                .foregroundStyle(
                    hovering
                        ? Theme.foreground
                        : Theme.foreground.opacity(0.75)
                )
                .frame(width: 26, height: Theme.sessionRowHeight)
                .background {
                    RoundedRectangle(cornerRadius: 7, style: .continuous)
                        .fill(hovering ? Theme.hoverRow : .clear)
                }
                .contentShape(
                    RoundedRectangle(cornerRadius: 7, style: .continuous)
                )
        }
        .buttonStyle(.plain)
        .onHover { hovering = $0 }
        .help(help)
        .accessibilityLabel(label)
        .animation(.easeInOut(duration: 0.12), value: hovering)
    }
}

// MARK: - Fit to desktop (phone resize) corner control

/// Ghost glyph shown while a phone temporarily drives this session's
/// terminal size (PhoneResizeOverride). Clicking reverts to the desktop's
/// natural grid — the same action the former "Resized for phone" banner
/// offered, now living in the pane's top-right corner.
struct PaneFitToDesktopButton: View {
    let grid: PhoneResizeOverride
    let onRevert: () -> Void
    @State private var hovering = false

    var body: some View {
        Button(action: onRevert) {
            Image(systemName: "iphone.slash")
                .font(.system(size: 10.5, weight: .semibold))
                .foregroundStyle(hovering ? Theme.foreground : Theme.mutedForeground)
                .frame(width: 24, height: 22)
                .contentShape(Rectangle())
        }
        .buttonStyle(.plain)
        .onHover { hovering = $0 }
        .animation(.easeInOut(duration: 0.12), value: hovering)
        .help("Resized for phone (\(grid.cols)×\(grid.rows)) — fit to desktop")
        .accessibilityLabel("Fit to desktop")
    }
}
