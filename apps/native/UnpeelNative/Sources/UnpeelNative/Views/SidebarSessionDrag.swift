//
//  SidebarSessionDrag.swift
//  UnpeelNative
//
//  Detached drag ("Dia feel") for sidebar SESSION and PROJECT rows — step 2 of
//  docs/plans/sidebar-drag-and-split.md.
//
//  Rows no longer start a system `.onDrag` session. Instead a local NSEvent
//  monitor watches mouse-down anywhere on a registered row; ~6pt of movement
//  lifts the row out as a floating card. The card is a borderless,
//  transparent window owned by this controller (not a SwiftUI overlay, and
//  deliberately NOT a child window — it is ordered above the parent
//  explicitly, so no parent/child frame coupling can interfere with
//  per-move `setFrameOrigin`), so the card is cursor-locked with zero
//  SwiftUI invalidation on the hot path (the previous @Published-origin
//  overlay re-entered the SwiftUI transaction machinery per event and
//  trailed the cursor). The card's row content is SNAPSHOTTED once at lift
//  — it never queries the store per frame. The original slot dims with a
//  spring and stays in the list as the live insertion gap.
//
//  NOTHING in the drag depends on mouse events reaching the local monitor
//  after the initial mouse-down: a 60Hz timer polls `NSEvent.mouseLocation`
//  (screen truth, always available) from the moment a press arms, observing
//  the lift slop while `.pressed` and driving the same move/hit-test path
//  while `.dragging`, with `NSEvent.pressedMouseButtons` as the
//  release/disarm fallback. Local monitors are documented to miss events
//  consumed by nested event-tracking loops (control tracking, window moves,
//  system drag sessions) — and the row's mouse-down is deliberately passed
//  through to SwiftUI, so whatever starts tracking on that press can starve
//  the monitor of every later event: mid-drag starvation froze the card at
//  its lift point (the shipped 2026-08-18 bug), and press-phase starvation
//  swallowed the lift entirely (no card at all — the press stayed a plain
//  click). A queued mouse-up while the physical button is still down is a
//  tracking-cancel artifact and never ends the press/drag; the poll ends it
//  on the true release. Events, when they do arrive, remain the low-latency
//  primary driver.
//
//  Hit-testing runs against row frames collected per-row via
//  `onGeometryChange` into a plain registry — no published state. The
//  move-as-you-pass gap inside the origin list is a TRANSFORM, not a tree
//  reorder: the section's order and slot stride are frozen at lift, hovering
//  a sibling row publishes one small target index, and each row's leaf
//  modifier offsets itself one slot toward the origin — the store is never
//  touched while the pointer moves (running `previewSessionMove` per crossed
//  row rebuilt the whole project tree and re-evaluated every sidebar row per
//  mouse move; large sidebars dropped frames the moment the card crossed the
//  list). `canMoveSession` still drives group/root drop-target highlighting,
//  and on mouse-up ONE `previewSessionMove`/`previewPinnedSessionMove` +
//  `commitSessionReorder`/`moveSession` applies what the transforms showed.
//  Esc cancels (the card springs back into its slot; the gap closes).
//
//  On top of that, two cross-group affordances:
//  - SPRING-OPEN: hovering a collapsed project/group/folder row for ~600ms
//    auto-expands it (standard spring-loading) so its contents become drop
//    targets mid-drag. Leaving the row before the delay cancels the pending
//    open; nothing auto-collapses afterwards.
//  - POSITIONAL CROSS-GROUP DROP: hovering a session row inside ANOTHER
//    group whose target accepts the session (`canMoveSession`, same-section,
//    not date-sorted) shows an insertion bar in that gap; releasing there
//    composes the existing verbs — `moveSession` files the session into the
//    group, then `setSessionOrder`/`setPinnedOrder` places it at the gap.
//
//  Perf discipline (same as the workspace swipe-peek): the heavy sidebar
//  tree holds the controller as plain `@State`/Environment value with NO
//  subscription. Per-mouse-move work is one dictionary hit-test plus a
//  direct AppKit window move; published `SidebarDragState` values only
//  change when the hovered row identity changes.
//

import AppKit
import QuartzCore
import SwiftUI

// MARK: - Controller

/// Per-move lean of the visible split drop-zone square toward the dragged
/// card (the mirror of the card's own magnet). Its OWN tiny observable so
/// the per-mouse-move churn re-renders only the square — never the store's
/// observers or the sidebar tree.
@MainActor
final class TerminalDropZoneMagnetState: ObservableObject {
    @Published private(set) var offsetX: CGFloat = 0

    func setOffsetX(_ offset: CGFloat) {
        // Sub-half-point wiggle isn't visible; don't publish it.
        guard abs(offset - offsetX) > 0.5 || (offset == 0 && offsetX != 0)
        else { return }
        offsetX = offset
    }
}

/// Visibility for the empty project-sidebar destination. This stays off the
/// store's hot observation path for the same reason as the split-zone magnet:
/// only the tiny overlay needs to redraw as a pane drag crosses the target.
@MainActor
final class ProjectSidebarPinDropState: ObservableObject {
    /// Eligible empty-sidebar destination. Kept visible at rest during the
    /// pane drag so the user never has to discover an invisible hit area.
    @Published private(set) var isAvailable = false
    @Published private(set) var isTargeted = false
    /// SwiftUI-space lean of the centered Pin glyph toward the cursor. The
    /// detached card and exact final sidebar footprint both stay fixed.
    @Published private(set) var cursorLean: CGSize = .zero
    /// Exact pane-card width the current project's persisted sidebar width
    /// will produce after the drop (the panel itself keeps an 8pt inset on
    /// both sides). The target morphs to this footprint while hovered.
    @Published private(set) var previewPaneWidth =
        Theme.sidebarDefaultWidth - Theme.surfaceInset * 2

    func set(
        available: Bool,
        targeted: Bool,
        cursorLean: CGSize = .zero,
        previewPaneWidth: CGFloat? = nil
    ) {
        let resolvedTargeted = available && targeted
        let resolvedLean = resolvedTargeted ? cursorLean : .zero
        if isAvailable != available { isAvailable = available }
        if isTargeted != resolvedTargeted { isTargeted = resolvedTargeted }
        if let previewPaneWidth,
           abs(self.previewPaneWidth - previewPaneWidth) > 0.5 {
            self.previewPaneWidth = previewPaneWidth
        }
        if abs(self.cursorLean.width - resolvedLean.width) > 0.5
            || abs(self.cursorLean.height - resolvedLean.height) > 0.5
            || (resolvedLean == .zero && self.cursorLean != .zero) {
            self.cursorLean = resolvedLean
        }
    }
}

@MainActor
final class SidebarSessionDragController: ObservableObject {
    /// Content-space coordinate system shared by the row-frame registry and
    /// the event monitor view (both anchored to the padded LazyVStack).
    nonisolated static let contentSpace = "sidebar-session-drag-content"

    /// What a registered sidebar row is, for hit-testing purposes.
    enum RowKind: Equatable {
        /// A reorderable session row (its section and depth captured so the
        /// floating card can be rebuilt faithfully).
        case session(projectID: String, pinned: Bool, depth: Int)
        /// A child group/worktree sitting in the parent's mixed session
        /// list — live-reorders with sessions, and is itself draggable.
        case folderItem(parentID: String, pinned: Bool, depth: Int)
        /// A top-level project row. It is both a project-reorder source/target
        /// and a potential session move target.
        case project
        /// The full visible block owned by a top-level project (header plus
        /// expanded descendants). It is a reorder TARGET but never a source:
        /// presses on a nested Session/group must retain their own meaning.
        case projectShell(projectID: String)
        /// The full visible background shell around a group. It gets a
        /// separate registry id and lower hit priority, so its header remains
        /// a draggable `folderItem` and contained Session rows retain their
        /// positional-reorder geometry while every bit of group wash still
        /// accepts a drop.
        case groupShell(projectID: String)
    }

    /// The lifted row rendered by the floating card. Row chrome that would
        /// otherwise need store lookups (selection and unread state) is
    /// snapshotted ONCE here at lift, so the card never touches the store
    /// per frame.
    struct Card {
        let session: SessionEntry?
        let folderName: String?
        let projectName: String?
        let projectFolderColor: ProjectFolderColor?
        let projectExpanded: Bool
        let projectID: String
        let pinned: Bool
        let depth: Int
        let size: CGSize
        let isSelected: Bool
        let isUnread: Bool
    }

    private struct RowRecord {
        var kind: RowKind
        var frame: CGRect
        /// Session rows also participate in immediate press selection even
        /// when their current sort/scope does not permit a detached drag.
        /// Other row kinds retain the historical draggable default.
        var isDraggable: Bool
        /// Identity of the reporter instance that registered this frame.
        /// A moved row (cross-group drop) is a NEW view for the same id:
        /// the new reporter registers before the old view's teardown runs,
        /// and an unscoped remove would delete the fresh registration —
        /// leaving the row undraggable until its list rebuilt.
        var token: UUID
    }

    /// A terminal pane title is another source for the same detached Session
    /// card. Keeping the backing NSView lets us query its live window-space
    /// frame while pane widths and visual order change under the drag.
    private final class PaneSourceRecord {
        weak var view: NSView?
        var session: SessionEntry
        /// Nil for the ordinary solo pane. Solo panes still need a drag
        /// source so they can file into the project sidebar; only sibling
        /// swapping depends on actual pane-group membership.
        var groupID: String?
        var paneID: String
        let token: UUID

        init(
            view: NSView,
            session: SessionEntry,
            groupID: String?,
            paneID: String,
            token: UUID
        ) {
            self.view = view
            self.session = session
            self.groupID = groupID
            self.paneID = paneID
            self.token = token
        }
    }

    private struct PaneDragContext {
        /// Nil for a solo pane. The context still identifies this as a pane
        /// title drag (and therefore enables the empty-sidebar Pin target).
        let groupID: String?
        let paneID: String
        let sessionID: String
        var lastTargetPaneID: String?
    }

    /// A pin anchors a row to its current sidebar partition, not to the
    /// sidebar itself. Pinned Sessions can still lift, reorder within the
    /// owning group's pinned partition, and become panes. That partition is
    /// ONE mixed list per parent project: pinned Sessions and pinned child
    /// groups reorder freely with each other. Only a sidebar drop outside
    /// the partition is refused.
    enum PinnedDragBoundary: Equatable {
        case partition(parentID: String)
    }

    enum PinnedSidebarHitAction: Equatable {
        /// Keep the current gap (for blank space inside the owning shell).
        case allow
        /// Move the gap to this row in the pinned partition.
        case reorder(anchorID: String)
        /// This row is outside the source's pinned partition.
        case deny
    }

    private enum Phase {
        case idle
        /// Mouse-down landed on a session row; waiting for the ~6pt slop.
        case pressed(sessionID: String, startLocation: NSPoint)
        case dragging
        /// Mouse-up/Esc happened; the card is animating into its slot.
        case settling
    }

    // Plumbing — deliberately not published (nothing observes this
    // controller; the card lives in its own AppKit window).
    private(set) weak var store: UnpeelStore?
    private(set) weak var dragState: SidebarDragState?
    /// True while the workspace carousel owns the list surface (container
    /// translated by a swipe, or a slide/commit in flight). Row frames are
    /// registered in the live page's UNTRANSLATED content space, so while
    /// the container is offset the registry and the monitor view's AppKit
    /// geometry disagree — never arm a session drag then. Set by
    /// SidebarView, which owns both the pager and this controller.
    var isSurfaceBusy: (() -> Bool)?
    private weak var monitorView: NSView?
    private var rows: [String: RowRecord] = [:]
    private var paneSources: [String: PaneSourceRecord] = [:]
    private var paneDragContext: PaneDragContext?
    private var pinnedDragBoundary: PinnedDragBoundary?
    private var pinnedDropDenied = false
    /// The pointer rests on a row owned by another git checkout (a worktree
    /// Session over its parent/root/another group, or any Session over a
    /// worktree it does not run in). Nothing highlights there, and a release
    /// returns the row home with the "no" shake (`SessionMoveRules`).
    private var worktreeDropDenied = false
    /// Mouse-down selects a Session immediately for fast navigation. If that
    /// press becomes a drag, restore the selection that was visibly hosting
    /// the terminal before the press so it remains the Split Pane target.
    private var selectionBeforePressedSession: String?
    private var phase: Phase = .idle
    private var lastWindowLocation: NSPoint = .zero
    private var hoveredTargetProjectID: String?
    private var hoveredInsertion: SidebarDragState.SessionInsertion?
    /// Same-section row whose slot the transform gap currently occupies
    /// (the dragged row's own id = gap at home). Retained across the small
    /// non-row gaps like `projectDropTargetID`; drives
    /// `SidebarDragState.sessionReorderTargetIndex` and, on drop, the ONE
    /// `previewSessionMove` call that actually reorders the tree.
    private var reorderAnchorID: String?
    /// Stable top-level reorder target. Project rows can be very tall while
    /// expanded; retaining the last real block across the 1pt stack gaps
    /// makes the drop reliable without rebuilding the tree during the drag.
    private var projectDropTargetID: String?
    private var projectDropBelow = false
    private var autoScrollTimer: Timer?
    private var autoScrollStep: CGFloat = 0
    /// The cursor-lock timer: polls `NSEvent.mouseLocation` from press
    /// through drag so the lift AND the card keep working even when
    /// dragged/up events are consumed away from the local monitor (see
    /// header). Runs during `.pressed` and `.dragging`.
    private var dragTimer: Timer?
    /// Content-area edge the card currently hovers (nil = none).
    private var hoveredDropZone: PaneDropTarget?
    /// The visible drop-zone square's lean toward the card (see
    /// `TerminalDropZoneMagnetState`); observed by the zone overlay only.
    let dropZoneMagnet = TerminalDropZoneMagnetState()
    /// Empty right-panel destination. An open panel uses its real pane frames
    /// as drop targets; this state exists only so the first pinned pane has a
    /// visible place to land.
    let projectSidebarPinDropState = ProjectSidebarPinDropState()
    /// The current root project's real persisted panel width. RootView owns
    /// that setting and reports it here without publishing through the store.
    private var projectSidebarWidth = Theme.sidebarDefaultWidth
    /// Live compact trigger frame. Non-nil means the Pin destination owns the
    /// drop; its expanded preview footprint supplies sticky hover hysteresis.
    private var projectSidebarPinDropRect: NSRect?
    /// Bumped on every begin/settle so stale scheduled cleanups no-op.
    private var generation = 0

    // Floating card (its own AppKit window; see header note).
    private var card: Card?
    private var cardWindow: NSWindow?
    private var cardState: SidebarSessionDragCardState?
    /// Cursor offset from the card's bottom-left, in window base coords
    /// (y-up), so the card stays under the grab point.
    private var grabOffset: NSPoint = .zero

    // Spring-loading (auto-expand of collapsed project/group rows).
    private var springOpenCandidateID: String?
    private var springOpenWork: DispatchWorkItem?

    // Feel parameters.
    static let dragThreshold: CGFloat = 6
    static let liftScale: CGFloat = 1.03
    static let liftAnimation: Animation = .spring(response: 0.28, dampingFraction: 0.75)
    static let settleAnimation: Animation = .spring(response: 0.32, dampingFraction: 0.85)
    static let settleDelay: TimeInterval = 0.3

    /// The card's landing flight (settle home, land in a reorder gap): a
    /// slight overshoot past the slot with a soft return — the window
    /// animator can't run true spring physics, but a y>1 control point
    /// reads as one. Ends exactly on the slot at the settle deadline.
    static func settleTimingFunction() -> CAMediaTimingFunction {
        CAMediaTimingFunction(controlPoints: 0.26, 1.16, 0.34, 1.0)
    }
    static let slotAnimation: Animation = .spring(response: 0.32, dampingFraction: 0.8)
    static let autoScrollZone: CGFloat = 44
    /// Max scroll speed near the edge, points per 1/60s tick (~780 pt/s).
    static let autoScrollMaxStep: CGFloat = 13
    /// Standard spring-loading dwell before a collapsed row auto-expands.
    static let springOpenDelay: TimeInterval = 0.6
    /// Padding around the card content inside its window so the lift shadow
    /// and 1.03 scale never clip at the window edge.
    static let cardWindowMargin: CGFloat = 40

    func bind(store: UnpeelStore, dragState: SidebarDragState, monitorView: NSView?) {
        self.store = store
        self.dragState = dragState
        if let monitorView { self.monitorView = monitorView }
    }

    func setProjectSidebarWidth(_ width: CGFloat) {
        projectSidebarWidth = min(
            Theme.sidebarMaxWidth,
            max(Theme.sidebarMinWidth, width)
        )
    }

    // MARK: Row-frame registry

    func setRowFrame(
        id: String,
        kind: RowKind,
        frame: CGRect,
        isDraggable: Bool = true,
        token: UUID
    ) {
        rows[id] = RowRecord(
            kind: kind,
            frame: frame,
            isDraggable: isDraggable,
            token: token
        )
    }

    /// Scoped to the registering reporter: a moved row's OLD view tears down
    /// after the new one registered the same id, and must not delete it.
    func removeRowFrame(id: String, token: UUID) {
        guard rows[id]?.token == token else { return }
        rows.removeValue(forKey: id)
    }

    /// Whether selecting a registered Session row needs a reveal scroll.
    /// Missing geometry fails toward revealing: the row may be outside the
    /// LazyVStack's mounted region. Rows already clear of the titlebar veil
    /// and bottom fade avoid an otherwise costly animated `scrollTo` pass.
    func sessionRowNeedsReveal(
        _ sessionID: String,
        topOcclusion: CGFloat,
        edgeMargin: CGFloat
    ) -> Bool {
        guard let record = rows[sessionID],
              case .session = record.kind,
              let scrollView = monitorView?.enclosingScrollView
        else { return true }
        return Self.rowNeedsReveal(
            rowFrame: record.frame,
            visibleRect: scrollView.documentVisibleRect,
            topOcclusion: topOcclusion,
            edgeMargin: edgeMargin
        )
    }

    /// Pure vertical reveal geometry shared with focused regression tests.
    /// Clamp the comfort margin when a row nearly fills the viewport so the
    /// row can still count as visible instead of requesting an impossible
    /// inset on every selection.
    nonisolated static func rowNeedsReveal(
        rowFrame: CGRect,
        visibleRect: CGRect,
        topOcclusion: CGFloat,
        edgeMargin: CGFloat
    ) -> Bool {
        let topInset = min(max(0, topOcclusion), max(0, visibleRect.height))
        let safeHeight = max(0, visibleRect.height - topInset)
        let margin = min(
            max(0, edgeMargin),
            max(0, (safeHeight - rowFrame.height) / 2)
        )
        let safeMinY = visibleRect.minY + topInset + margin
        let safeMaxY = visibleRect.maxY - margin
        return rowFrame.minY < safeMinY || rowFrame.maxY > safeMaxY
    }

    func registerPaneSource(
        view: NSView,
        session: SessionEntry,
        groupID: String?,
        paneID: String,
        token: UUID
    ) {
        paneSources[paneID] = PaneSourceRecord(
            view: view,
            session: session,
            groupID: groupID,
            paneID: paneID,
            token: token
        )
    }

    func removePaneSource(paneID: String, token: UUID) {
        guard paneSources[paneID]?.token == token else { return }
        paneSources.removeValue(forKey: paneID)
    }

    /// A mounted pane's live content frame, hit-tested during a Session drag
    /// to offer 4-sided pane-edge splits. A solo pane registers with
    /// `isSolo` — its edges resolve to group-edge targets because the solo
    /// Session has no pane in the layout yet.
    private final class PaneDropTargetRecord {
        weak var view: NSView?
        var paneID: String
        var isSolo: Bool
        let token: UUID

        init(view: NSView, paneID: String, isSolo: Bool, token: UUID) {
            self.view = view
            self.paneID = paneID
            self.isSolo = isSolo
            self.token = token
        }
    }

    private var paneDropTargets: [String: PaneDropTargetRecord] = [:]

    func registerPaneDropTarget(
        view: NSView,
        paneID: String,
        isSolo: Bool,
        token: UUID
    ) {
        paneDropTargets[paneID] = PaneDropTargetRecord(
            view: view,
            paneID: paneID,
            isSolo: isSolo,
            token: token
        )
    }

    func removePaneDropTarget(paneID: String, token: UUID) {
        guard paneDropTargets[paneID]?.token == token else { return }
        paneDropTargets.removeValue(forKey: paneID)
    }

    /// Called by the pane-title chip once SwiftUI's 6pt drag gesture lifts.
    /// From this point the existing cursor-lock timer, floating Session card,
    /// sidebar hit-testing, insertion gaps, and spring-loading own the drag.
    func beginPaneTitleDrag(
        sessionID: String,
        groupID: String?,
        paneID: String
    ) {
        guard case .idle = phase,
              dragState?.isActive != true,
              let store,
              let dragState,
              let monitorView,
              let parent = monitorView.window,
              let source = paneSources[paneID],
              source.groupID == groupID,
              source.session.id == sessionID,
              let sourceView = source.view,
              sourceView.window === parent,
              let location = sidebarLocation(for: sessionID)
        else { return }

        let rowRect = sourceView.convert(sourceView.bounds, to: nil)
        let pointer = parent.convertPoint(fromScreen: NSEvent.mouseLocation)
        let cardSize = CGSize(
            width: min(max(rowRect.width + 72, 180), 280),
            height: Theme.sessionRowHeight
        )
        let newCard = Card(
            session: source.session,
            folderName: nil,
            projectName: nil,
            projectFolderColor: nil,
            projectExpanded: false,
            projectID: location.projectID,
            pinned: location.pinned,
            depth: location.depth,
            size: cardSize,
            isSelected: store.selectedSessionID == sessionID,
            isUnread: store.sessionIsUnread(sessionID)
        )

        generation += 1
        selectionBeforePressedSession = nil
        pinnedDragBoundary = location.pinned
            ? .partition(parentID: location.projectID)
            : nil
        pinnedDropDenied = false
        worktreeDropDenied = false
        dragState.beginSession(
            projectID: location.projectID,
            sessionID: sessionID,
            pinned: location.pinned,
            armed: false,
            commitReorder: { [weak store] in
                store?.commitSessionReorder(
                    projectID: location.projectID,
                    pinned: location.pinned
                )
            },
            cancelReorder: { [weak store] in
                store?.cancelSessionReorder(projectID: location.projectID)
            }
        )
        dragState.setLiftedSessionRow(sessionID)
        dragState.setListGlideSuppressed(true)
        paneDragContext = PaneDragContext(
            groupID: source.groupID,
            paneID: source.paneID,
            sessionID: sessionID
        )
        lastWindowLocation = pointer
        grabOffset = NSPoint(
            x: min(max(pointer.x - rowRect.minX, 0), cardSize.width),
            y: min(max(pointer.y - rowRect.minY, 0), cardSize.height)
        )
        presentCardWindow(card: newCard, over: parent, rowRectInWindow: rowRect)
        card = newCard
        phase = .dragging
        hoveredTargetProjectID = nil
        hoveredInsertion = nil
        reorderAnchorID = nil
        projectDropTargetID = nil
        projectDropBelow = false
        springOpenCandidateID = nil
        startDragTimer()
        trackCursor(in: monitorView)
    }

    /// SwiftUI's title gesture normally ends first; the 60Hz physical-button
    /// fallback may win when an event-tracking loop consumes that callback.
    func endPaneTitleDrag() {
        guard case .dragging = phase,
              paneDragContext != nil,
              let monitorView,
              let parent = monitorView.window
        else { return }
        lastWindowLocation = parent.convertPoint(fromScreen: NSEvent.mouseLocation)
        endDrag(in: monitorView)
    }

    // MARK: Project-sidebar (right panel) pane drags

    /// Right-panel pane frames, keyed by session id. They serve two roles:
    /// the drag's source card geometry, and hover targets for live reordering
    /// of the panel stack.
    private final class PanelPaneSourceRecord {
        weak var view: NSView?
        let token: UUID

        init(view: NSView, token: UUID) {
            self.view = view
            self.token = token
        }
    }

    private var panelPaneSources: [String: PanelPaneSourceRecord] = [:]
    /// Session id of a drag started from a panel pane. Unlike a pane-title
    /// drag there is no `paneDragContext` — the session has no pane-layout
    /// membership; this is an ordinary sidebar-session drag whose row lives in
    /// the project's Sidebar group.
    private var panelDragSessionID: String?
    /// Last hover-reorder target, re-armed when the cursor leaves all panes.
    private var panelReorderTargetID: String?
    /// Non-member pin drop: the hovered pane + which half (insert above or
    /// below it), mirroring the main area's edge-split targeting.
    private var panelInsertTarget: (sessionID: String, below: Bool)?

    func registerPanelPaneSource(view: NSView, sessionID: String, token: UUID) {
        panelPaneSources[sessionID] = PanelPaneSourceRecord(view: view, token: token)
    }

    func removePanelPaneSource(sessionID: String, token: UUID) {
        guard panelPaneSources[sessionID]?.token == token else { return }
        panelPaneSources.removeValue(forKey: sessionID)
    }

    /// Begin an ordinary session drag from a right-panel pane header — the
    /// same detached card, sidebar insertion/group targets, and main-area
    /// split zones as dragging the session's own sidebar row.
    func beginPanelPaneDrag(sessionID: String) {
        guard case .idle = phase,
              dragState?.isActive != true,
              let store,
              let dragState,
              let monitorView,
              let parent = monitorView.window,
              let source = panelPaneSources[sessionID],
              let sourceView = source.view,
              sourceView.window === parent,
              let session = store.displaySessionsByID[sessionID],
              let location = sidebarLocation(for: sessionID)
        else { return }

        let rowRect = sourceView.convert(sourceView.bounds, to: nil)
        let pointer = parent.convertPoint(fromScreen: NSEvent.mouseLocation)
        let cardSize = CGSize(width: 240, height: Theme.sessionRowHeight)
        let newCard = Card(
            session: session,
            folderName: nil,
            projectName: nil,
            projectFolderColor: nil,
            projectExpanded: false,
            projectID: location.projectID,
            pinned: location.pinned,
            depth: location.depth,
            size: cardSize,
            isSelected: store.selectedSessionID == sessionID,
            isUnread: store.sessionIsUnread(sessionID)
        )

        generation += 1
        selectionBeforePressedSession = nil
        pinnedDragBoundary = location.pinned
            ? .partition(parentID: location.projectID)
            : nil
        pinnedDropDenied = false
        worktreeDropDenied = false
        dragState.beginSession(
            projectID: location.projectID,
            sessionID: sessionID,
            pinned: location.pinned,
            armed: false,
            commitReorder: { [weak store] in
                store?.commitSessionReorder(
                    projectID: location.projectID,
                    pinned: location.pinned
                )
            },
            cancelReorder: { [weak store] in
                store?.cancelSessionReorder(projectID: location.projectID)
            }
        )
        dragState.setLiftedSessionRow(sessionID)
        dragState.setListGlideSuppressed(true)
        panelDragSessionID = sessionID
        panelReorderTargetID = nil
        lastWindowLocation = pointer
        grabOffset = NSPoint(
            x: min(max(pointer.x - rowRect.minX, 0), cardSize.width),
            y: min(max(pointer.y - rowRect.minY, 0), cardSize.height)
        )
        presentCardWindow(card: newCard, over: parent, rowRectInWindow: rowRect)
        card = newCard
        phase = .dragging
        hoveredTargetProjectID = nil
        hoveredInsertion = nil
        reorderAnchorID = nil
        projectDropTargetID = nil
        projectDropBelow = false
        springOpenCandidateID = nil
        startDragTimer()
        trackCursor(in: monitorView)
    }

    func endPanelPaneDrag() {
        guard case .dragging = phase,
              panelDragSessionID != nil,
              let monitorView,
              let parent = monitorView.window
        else { return }
        lastWindowLocation = parent.convertPoint(fromScreen: NSEvent.mouseLocation)
        endDrag(in: monitorView)
    }

    // MARK: Event handling (from the local monitor)

    /// Returns nil to swallow the event once a drag owns the mouse.
    func handle(_ event: NSEvent, in view: NSView) -> NSEvent? {
        switch event.type {
        case .leftMouseDown:
            return handleMouseDown(event, in: view)
        case .leftMouseDragged:
            return handleMouseDragged(event, in: view)
        case .leftMouseUp:
            return handleMouseUp(event, in: view)
        case .keyDown:
            return handleKeyDown(event)
        default:
            return event
        }
    }

    private func handleMouseDown(_ event: NSEvent, in view: NSView) -> NSEvent? {
        guard case .idle = phase,
              dragState?.isActive != true,
              // Mid workspace swipe/slide the registry's untranslated frames
              // do not match on-screen geometry; the carousel owns the mouse.
              isSurfaceBusy?() != true,
              // Control-click is the context menu, never a drag.
              !event.modifierFlags.contains(.control)
        else { return event }
        selectionBeforePressedSession = nil
        let point = view.convert(event.locationInWindow, from: nil)
        // Only arm a press for a row the user can actually see: content-space
        // hit-testing would otherwise match rows scrolled under the titlebar
        // chrome (a window drag over the sidebar) or below the viewport.
        if let scrollView = view.enclosingScrollView {
            var visible = scrollView.documentVisibleRect
            visible.origin.y += Theme.titlebarHeight
            visible.size.height -= Theme.titlebarHeight
            guard visible.contains(point) else { return event }
        }
        // SwiftUI's TapGesture commits on mouse-up. That makes navigation
        // feel one physical click behind the keyboard-driven TUI even when
        // the selected pane is already warm. The detached-drag monitor sees
        // the same press first, so select from mouse-down in the row's
        // non-control region and let the ordinary tap remain as a fallback.
        // If the press becomes a drag, beginDrag restores the prior terminal
        // so that visible Session remains the Split Pane destination.
        // The trailing cluster is reserved for Resume/Archive/runtime
        // controls; confirmation and edit modes own the whole row.
        let selectionBeforePress = store?.selectedSessionID
        if let (id, record) = sessionRow(at: point),
           Self.immediateSelectionHit(
               point: point,
               rowFrame: record.frame,
               trailingControlWidth: Self.immediateSelectionTrailingControlWidth
           ),
           sessionAcceptsImmediateSelection(id) {
            store?.selectedSessionID = id
        }
        guard let (id, record) = draggableRow(at: point) else { return event }
        guard Self.shouldArmRowDrag(
            rowID: id,
            editingSessionID: store?.editingSessionID,
            confirmingRemoveSessionID: store?.confirmingRemoveSessionID
        ) else { return event }
        if case .session = record.kind {
            selectionBeforePressedSession = selectionBeforePress
        }
        phase = .pressed(sessionID: id, startLocation: event.locationInWindow)
        // The lift must NOT depend on dragged events reaching the local
        // monitor: whatever event-tracking the row press starts (see header)
        // can consume every subsequent dragged event, in which case the 6pt
        // slop would never be observed and the row would never lift — the
        // press would silently stay a plain click, with no card. The same
        // 60Hz cursor poll that keeps a lifted card tracking therefore ALSO
        // watches the armed press: real cursor travel lifts, real button
        // release disarms. Events, when they arrive, remain the low-latency
        // primary driver.
        startDragTimer()
        return event
    }

    /// Width reserved for the hover action(s), age/runtime mark, and their
    /// padding. Clicks there continue through SwiftUI so a Button never
    /// changes tabs as a side effect; the title/leading/body region selects
    /// immediately.
    static let immediateSelectionTrailingControlWidth: CGFloat = 76

    nonisolated static func immediateSelectionHit(
        point: CGPoint,
        rowFrame: CGRect,
        trailingControlWidth: CGFloat
    ) -> Bool {
        guard rowFrame.contains(point) else { return false }
        // Keep at least half of a very narrow row available for selection.
        let exclusion = min(
            max(0, trailingControlWidth),
            max(0, rowFrame.width / 2)
        )
        return point.x <= rowFrame.maxX - exclusion
    }

    private func sessionAcceptsImmediateSelection(_ sessionID: String) -> Bool {
        guard let store else { return false }
        if store.editingSessionID == sessionID,
           store.editingSessionSurface == .sidebar {
            return false
        }
        if store.confirmingRemoveSessionID == sessionID,
           store.confirmingRemoveSurface == .sidebar {
            return false
        }
        if store.confirmingArchiveSessionID == sessionID { return false }
        return !store.removingSessionIDs.contains(sessionID)
            && !store.restartingSessionIDs.contains(sessionID)
            && !store.resumingAgentSessionIDs.contains(sessionID)
            && !store.archivingSessionIDs.contains(sessionID)
    }

    private func handleMouseDragged(_ event: NSEvent, in view: NSView) -> NSEvent? {
        switch phase {
        case let .pressed(sessionID, start):
            guard Self.liftDecision(
                buttonPressed: true,
                start: start,
                current: event.locationInWindow,
                threshold: Self.dragThreshold
            ) == .lift else { return event }
            return beginDrag(
                sessionID: sessionID, location: event.locationInWindow, in: view
            ) ? nil : event
        case .dragging:
            lastWindowLocation = event.locationInWindow
            trackCursor(in: view)
            return nil
        case .idle, .settling:
            return event
        }
    }

    private func handleMouseUp(_ event: NSEvent, in view: NSView) -> NSEvent? {
        // A queued mouse-up while the physical button is still down is a
        // tracking-cancel artifact, not the user's release (a real release
        // updates `pressedMouseButtons` before the event is delivered). The
        // press/drag stays alive; the 60Hz poll ends it on the true release.
        let physicallyDown = NSEvent.pressedMouseButtons & 0x1 == 0x1
        switch phase {
        case .pressed:
            if !physicallyDown {
                stopDragTimer()
                phase = .idle
                selectionBeforePressedSession = nil
            }
            return event
        case .dragging:
            guard !physicallyDown else { return nil }
            lastWindowLocation = event.locationInWindow
            endDrag(in: view)
            return nil
        case .idle, .settling:
            return event
        }
    }

    private func handleKeyDown(_ event: NSEvent) -> NSEvent? {
        // Esc cancels an in-flight drag; while merely pressed it stays a
        // click-in-progress and passes through.
        guard case .dragging = phase, event.keyCode == 53 else { return event }
        settle(commit: false)
        return nil
    }

    /// The hosting view left the window mid-drag (pane switch): never leave
    /// the drag armed or the source row hidden. A still-open drag state
    /// (drag in flight, or a settle whose deferred finish/end hasn't run)
    /// cancels — the preview reverts to the persisted order.
    func monitorDetached() {
        guard let dragState else { return }
        switch phase {
        case .dragging, .settling:
            stopDragTimer()
            stopAutoScroll()
            cancelSpringOpen()
            clearHoveredTarget()
            clearInsertion()
            clearDropZone()
            generation += 1
            if dragState.isActive { dragState.end() }
            forceClearCard()
        case .pressed:
            stopDragTimer()
            phase = .idle
            selectionBeforePressedSession = nil
        case .idle:
            break
        }
    }

    /// The pure lift/disarm rule shared by the event path and the cursor
    /// poll: a released button dissolves the press, `dragThreshold` points
    /// of travel lift it, anything less holds.
    enum LiftDecision: Equatable {
        case hold
        case cancel
        case lift
    }

    nonisolated static func liftDecision(
        buttonPressed: Bool,
        start: NSPoint,
        current: NSPoint,
        threshold: CGFloat
    ) -> LiftDecision {
        guard buttonPressed else { return .cancel }
        let dx = current.x - start.x
        let dy = current.y - start.y
        return dx * dx + dy * dy >= threshold * threshold ? .lift : .hold
    }

    /// Rename and inline remove-confirm keep their row in the frame
    /// registry so leaving those modes cannot drop it from hit-testing.
    /// The press itself must still not lift: drags inside the title field
    /// are text selection, and the confirm buttons own that click.
    nonisolated static func shouldArmRowDrag(
        rowID: String,
        editingSessionID: String?,
        confirmingRemoveSessionID: String?
    ) -> Bool {
        rowID != editingSessionID && rowID != confirmingRemoveSessionID
    }

    /// Immediate mouse-down selection must not replace the terminal that a
    /// confirmed sidebar drag is about to split. Return the prior selection
    /// only when this exact press changed it to the dragged row; an unrelated
    /// selection change between press and lift wins.
    nonisolated static func selectionToRestoreForLift(
        draggedID: String,
        selectedBeforePress: String?,
        currentSelection: String?
    ) -> String? {
        guard let selectedBeforePress,
              selectedBeforePress != draggedID,
              currentSelection == draggedID
        else { return nil }
        return selectedBeforePress
    }

    // MARK: Drag lifecycle

    private func beginDrag(sessionID: String, location: NSPoint, in view: NSView) -> Bool {
        guard let store, let dragState,
              // A two-finger swipe can engage the carousel while a row press
              // is still armed; the lift geometry would be against a
              // translated page. The press dissolves into a plain click.
              isSurfaceBusy?() != true,
              let parent = view.window,
              let record = rows[sessionID],
              Self.shouldArmRowDrag(
                  rowID: sessionID,
                  editingSessionID: store.editingSessionID,
                  confirmingRemoveSessionID: store.confirmingRemoveSessionID
              )
        else {
            stopDragTimer()
            phase = .idle
            selectionBeforePressedSession = nil
            return false
        }
        if case .session = record.kind,
           let restored = Self.selectionToRestoreForLift(
               draggedID: sessionID,
               selectedBeforePress: selectionBeforePressedSession,
               currentSelection: store.selectedSessionID
           ) {
            store.selectedSessionID = restored
        }
        selectionBeforePressedSession = nil
        let projectID: String
        let pinned: Bool
        let newCard: Card
        pinnedDragBoundary = Self.pinnedBoundary(for: record.kind)
        pinnedDropDenied = false
        worktreeDropDenied = false
        switch record.kind {
        case let .session(ownerID, isPinned, rowDepth):
            guard let entry = sessionEntry(projectID: ownerID, sessionID: sessionID) else {
                stopDragTimer()
                phase = .idle
                pinnedDragBoundary = nil
                return false
            }
            projectID = ownerID
            pinned = isPinned
            newCard = Card(
                session: entry,
                folderName: nil,
                projectName: nil,
                projectFolderColor: nil,
                projectExpanded: false,
                projectID: ownerID,
                pinned: isPinned,
                depth: rowDepth,
                size: record.frame.size,
                isSelected: store.selectedSessionID == sessionID,
                isUnread: store.sessionIsUnread(sessionID)
            )
        case let .folderItem(parentID, isPinned, rowDepth):
            guard let name = folderName(parentID: parentID, folderID: sessionID) else {
                stopDragTimer()
                phase = .idle
                pinnedDragBoundary = nil
                return false
            }
            projectID = parentID
            // A pinned child group commits through the pinned-partition
            // path, exactly like a pinned Session row.
            pinned = isPinned
            newCard = Card(
                session: nil,
                folderName: name,
                projectName: nil,
                projectFolderColor: nil,
                projectExpanded: false,
                projectID: parentID,
                pinned: isPinned,
                depth: rowDepth,
                size: record.frame.size,
                isSelected: false,
                isUnread: false
            )
        case .project:
            guard let project = store.displayProjectsByID[sessionID] else {
                stopDragTimer()
                phase = .idle
                pinnedDragBoundary = nil
                return false
            }
            projectID = sessionID
            pinned = false
            newCard = Card(
                session: nil,
                folderName: nil,
                projectName: project.name,
                projectFolderColor: store.selectedHostScope == .local
                    ? store.projectFolderColor(for: sessionID)
                    : nil,
                projectExpanded: store.expandedProjectIDs.contains(sessionID),
                projectID: sessionID,
                pinned: false,
                depth: 0,
                size: record.frame.size,
                isSelected: false,
                isUnread: false
            )
        case .projectShell, .groupShell:
            stopDragTimer()
            phase = .idle
            pinnedDragBoundary = nil
            return false
        }
        generation += 1
        if case .project = record.kind {
            dragState.beginProject(
                sessionID,
                armed: false,
                commitReorder: { [weak store] in store?.commitProjectReorder() },
                cancelReorder: { [weak store] in store?.cancelProjectReorder() }
            )
            dragState.setLiftedProjectRow(sessionID)
        } else {
            dragState.beginSession(
                projectID: projectID,
                sessionID: sessionID,
                pinned: pinned,
                armed: false,
                commitReorder: { [weak store] in
                    store?.commitSessionReorder(projectID: projectID, pinned: pinned)
                },
                cancelReorder: { [weak store] in
                    store?.cancelSessionReorder(projectID: projectID)
                }
            )
            dragState.setLiftedSessionRow(sessionID)
            dragState.setListGlideSuppressed(true)
            // Freeze the origin section for the transform-gap reorder:
            // order from the store's rendered list (sessions and child
            // folders mixed, or the pinned section), stride from the
            // registered static row frames. During the drag nothing
            // reorders the tree, so both stay true until drop. An EXPANDED
            // folder as the drag source gets no map: its visible block is
            // taller than the one-slot stride, so the transform gap can't
            // represent its move — collapsed folders reorder normally, and
            // cross-project drops still work without a gap.
            let expandedFolderSource: Bool = {
                if case .folderItem = record.kind {
                    return store.expandedProjectIDs.contains(sessionID)
                }
                return false
            }()
            if !expandedFolderSource, let node = store.findDisplayNode(projectID) {
                let pinnedIDs = store.renderedPinnedItems(in: node).map(\.id)
                // The pinned partition freezes as ONE mixed list (pinned
                // Sessions and pinned child groups), so either row kind
                // opens its gap against any pinned sibling.
                let ids = pinned
                    ? pinnedIDs
                    : store.renderedDisplayedItems(in: node).map(\.id)
                dragState.beginSessionReorderMap(
                    ids: ids,
                    draggedID: sessionID,
                    shiftDistance: reorderStride(
                        ids: ids, draggedID: sessionID, frame: record.frame
                    ),
                    // The header-hover top anchor only when the header is
                    // ADJACENT to the dragged row's section: a pinned drag,
                    // or a regular drag with no pinned rows in between.
                    headerAnchorsTop: pinned || pinnedIDs.isEmpty
                )
            }
        }

        lastWindowLocation = location
        // Window base coords (y-up); `convert(_:to: nil)` folds in the
        // monitor view's flipped, scrolled content space.
        let rowRect = view.convert(record.frame, to: nil)
        // Clamp so a fast slop-exit can't leave the card hanging far from
        // the cursor.
        grabOffset = NSPoint(
            x: min(max(lastWindowLocation.x - rowRect.minX, 0), rowRect.width),
            y: min(max(lastWindowLocation.y - rowRect.minY, 0), rowRect.height)
        )
        // Present BEFORE recording the snapshot: presentCardWindow's initial
        // closeCardWindow() (previous-card teardown) clears `card`, and a
        // nil snapshot would freeze the new card at its spawn origin.
        presentCardWindow(card: newCard, over: parent, rowRectInWindow: rowRect)
        card = newCard
        phase = .dragging
        hoveredTargetProjectID = nil
        hoveredInsertion = nil
        reorderAnchorID = nil
        projectDropTargetID = nil
        projectDropBelow = false
        springOpenCandidateID = nil
        startDragTimer()
        trackCursor(in: view)
        return true
    }

    /// One slot's layout stride — the dragged row's height plus the list
    /// spacing to a section neighbor, measured from the registered frames.
    /// This is exactly how far the rows between the origin slot and the
    /// hovered target shift to open the gap.
    private func reorderStride(
        ids: [String], draggedID: String, frame: CGRect
    ) -> CGFloat {
        guard let index = ids.firstIndex(of: draggedID) else { return frame.height }
        if index + 1 < ids.count, let next = rows[ids[index + 1]] {
            return next.frame.minY - frame.minY
        }
        if index > 0, let prev = rows[ids[index - 1]] {
            return frame.minY - prev.frame.minY
        }
        return frame.height
    }

    // MARK: Cursor-lock timer

    /// 60Hz poll of the real cursor from press through drag. The local event
    /// monitor stays the low-latency driver; this timer guarantees the WHOLE
    /// lifecycle survives event starvation (dragged/up events consumed by a
    /// nested event-tracking loop the monitor never sees): while `.pressed`
    /// it observes the slop and LIFTS (or disarms on the real button
    /// release), and while `.dragging` it keeps the card/hit-testing
    /// tracking and finishes the drag if the release itself was swallowed.
    private func startDragTimer() {
        stopDragTimer()
        let timer = Timer(timeInterval: 1.0 / 60.0, repeats: true) { [weak self] _ in
            MainActor.assumeIsolated { self?.dragTimerTick() }
        }
        // .common so it fires during mouse-drag tracking, like autoScroll.
        RunLoop.main.add(timer, forMode: .common)
        dragTimer = timer
    }

    private func dragTimerTick() {
        guard let view = monitorView, let parent = view.window else {
            return stopDragTimer()
        }
        let buttonDown = NSEvent.pressedMouseButtons & 0x1 == 0x1
        let location = parent.convertPoint(fromScreen: NSEvent.mouseLocation)
        switch phase {
        case let .pressed(sessionID, start):
            // Same decision the dragged-event path makes — screen truth
            // instead of (possibly starved) events.
            switch Self.liftDecision(
                buttonPressed: buttonDown,
                start: start,
                current: location,
                threshold: Self.dragThreshold
            ) {
            case .hold:
                break
            case .cancel:
                stopDragTimer()
                phase = .idle
                selectionBeforePressedSession = nil
            case .lift:
                _ = beginDrag(sessionID: sessionID, location: location, in: view)
            }
        case .dragging:
            // The button is up but no mouse-up reached the monitor: finish
            // the drag from here (same routing as handleMouseUp).
            guard buttonDown else {
                endDrag(in: view)
                return
            }
            guard location != lastWindowLocation else { return }
            lastWindowLocation = location
            trackCursor(in: view)
        case .idle, .settling:
            stopDragTimer()
        }
    }

    private func stopDragTimer() {
        dragTimer?.invalidate()
        dragTimer = nil
    }

    private func trackCursor(in view: NSView) {
        guard case .dragging = phase else { return }
        moveCardWindow(in: view)
        let content = view.convert(lastWindowLocation, from: nil)
        // Same x-rule endDrag uses for commit-vs-cancel: while the card is
        // out over the terminal, the origin slot closes (no stray gap in
        // the sidebar); it reopens the moment the card returns.
        let insideSidebar = content.x >= 0 && content.x <= view.bounds.width
        if dragState?.sessionDrag != nil {
            dragState?.setLiftedSlotCollapsed(!insideSidebar)
            // The row "left the list": the same-section gap springs closed
            // alongside the collapsing origin slot; re-entering the sidebar
            // re-opens it at the next hit-tested row.
            if !insideSidebar { clearReorderTarget() }
        }
        updateProjectSidebarPinTarget(insideSidebar: insideSidebar, in: view)
        updatePaneReorder()
        updatePanelReorder()
        updateTerminalDropZone(insideSidebar: insideSidebar, in: view)
        hitTest(content, insideSidebar: insideSidebar)
        updateAutoScroll(content: content, in: view)
    }

    /// Hovering a right-panel pane during a session drag highlights it as
    /// the drop target (the same drop-zone preview frame the split zones
    /// use). For a panel MEMBER that means reorder-to-here; for any other
    /// pinnable session it means pin-into-the-panel at that position. The
    /// durable write happens on drop in `endDrag`.
    private func updatePanelReorder() {
        guard let store, let drag = dragState?.sessionDrag else { return }
        let isMember = store.sessionIsInProjectSidebar(drag.sessionID)
        guard isMember || store.canMoveSessionToProjectSidebar(drag.sessionID)
        else { return }
        var hovered: (sessionID: String, rect: NSRect)?
        for (sessionID, source) in panelPaneSources
        where sessionID != drag.sessionID {
            guard let view = source.view, view.window != nil else { continue }
            let rect = view.convert(view.bounds, to: nil)
            if rect.contains(lastWindowLocation) {
                hovered = (sessionID, rect)
                break
            }
        }
        if isMember {
            // Members reorder: full-frame preview, position on drop.
            let target = hovered?.sessionID
            guard target != panelReorderTargetID else { return }
            panelReorderTargetID = target
            store.setProjectSidebarReorderPreview(target)
            return
        }
        // Pinnable non-members insert: half-frame preview showing which side
        // of the hovered pane the new pane lands on (AppKit y-up: a cursor
        // above the midline means the UPPER half).
        let target = hovered.map { hit in
            (sessionID: hit.sessionID, below: lastWindowLocation.y < hit.rect.midY)
        }
        guard target?.sessionID != panelInsertTarget?.sessionID
            || target?.below != panelInsertTarget?.below
        else { return }
        panelInsertTarget = target
        store.setProjectSidebarInsertPreview(target.map {
            UnpeelStore.ProjectSidebarInsertPreview(
                sessionID: $0.sessionID, below: $0.below
            )
        })
    }

    /// Hovering another pane's title highlights it as the SWAP TARGET (the
    /// same drop-zone preview language as edge splits); the actual swap
    /// commits on drop in `endDrag`, never live mid-drag.
    private func updatePaneReorder() {
        guard var context = paneDragContext,
              let groupID = context.groupID,
              let store
        else { return }
        // The explicit green Pin square owns this point. Without this guard,
        // the rightmost sibling underneath it would also show a swap preview.
        guard projectSidebarPinDropRect == nil else {
            if context.lastTargetPaneID != nil {
                context.lastTargetPaneID = nil
                paneDragContext = context
                store.setPaneSwapPreview(nil)
            }
            return
        }
        var target: String?
        for (_, source) in paneSources
        where source.paneID != context.paneID
            && source.groupID == groupID {
            // The WHOLE sibling pane is the hover target, not just its title
            // strip: prefer the pane's full-card frame (already registered
            // for edge-split drops), falling back to the header registration.
            let view = paneDropTargets[source.paneID]?.view ?? source.view
            guard let view, view.window != nil else { continue }
            let rect = view.convert(view.bounds, to: nil)
            if rect.contains(lastWindowLocation) {
                target = source.paneID
                break
            }
        }
        guard target != context.lastTargetPaneID else { return }
        context.lastTargetPaneID = target
        paneDragContext = context
        store.setPaneSwapPreview(target)
    }

    /// Split drop zones: while an eligible drag rides over the content
    /// area's leading/trailing thirds, the store publishes the hovered zone
    /// (ContentArea renders the highlight); releasing there splits the shown
    /// terminal with the dragged session. Eligibility is the store's
    /// `canSplitTerminal` — both Sessions attachable in the selected Host
    /// scope, enough group capacity, and the terminal actually shown.
    /// Width of the content-edge band that maps to a group-edge (root) split
    /// instead of a pane-edge split.
    nonisolated static let groupEdgeBandWidth: CGFloat = 28
    /// The first project-sidebar member starts as a compact, unmistakable
    /// square, then morphs into its actual full-height pane footprint while
    /// hovered instead of borrowing the ordinary right-split preview.
    nonisolated static let projectSidebarPinTargetSize: CGFloat = 76
    nonisolated static let projectSidebarPinTargetTrailingInset: CGFloat = 4
    nonisolated static let projectSidebarPinTargetTopInset: CGFloat = 12
    /// A narrow runway joins the visible inset square to the literal window
    /// edge, so "all the way right" never overshoots the destination.
    nonisolated static let projectSidebarPinEdgeRunwayWidth: CGFloat = 32
    /// Cursor polling continues outside the content view during a detached
    /// drag. Preserve a small sticky overshoot beyond the window edge so a
    /// decisive throw right still lands instead of cancelling by one pixel.
    nonisolated static let projectSidebarPinEdgeOvershoot: CGFloat = 24
    nonisolated static let projectSidebarPinSquareLeanMax: CGFloat = 10
    /// Panes shorter than this reject up/down drops — a vertical split would
    /// leave no useful terminal under two stacked headers.
    nonisolated static let minimumVerticalSplitTargetHeight: CGFloat = 120

    /// Top-trailing square used when the current project's right panel has
    /// no members yet. `contentRect` is the union of the mounted main panes,
    /// so this geometry matches the SwiftUI overlay even with title strips or
    /// outer surface insets.
    nonisolated static func projectSidebarPinTargetRect(
        in contentRect: NSRect
    ) -> NSRect {
        NSRect(
            x: contentRect.maxX - projectSidebarPinTargetTrailingInset
                - projectSidebarPinTargetSize,
            y: contentRect.maxY - projectSidebarPinTargetTopInset
                - projectSidebarPinTargetSize,
            width: projectSidebarPinTargetSize,
            height: projectSidebarPinTargetSize
        )
    }

    /// Width of the pane card that will appear inside the right panel. The
    /// persisted panel width includes the same surface inset on both sides,
    /// so subtract them to preview the final visible card exactly.
    nonisolated static func projectSidebarPinPreviewPaneWidth(
        projectSidebarWidth: CGFloat
    ) -> CGFloat {
        max(0, projectSidebarWidth - Theme.surfaceInset * 2)
    }

    /// Full first-pane footprint after the Pin drop. Besides driving visual
    /// sizing, this becomes a hover-retention area after the compact square
    /// has expanded, preventing a collapse when the pointer explores it.
    nonisolated static func projectSidebarPinPreviewRect(
        in contentRect: NSRect,
        projectSidebarWidth: CGFloat
    ) -> NSRect {
        let width = min(
            contentRect.width,
            projectSidebarPinPreviewPaneWidth(
                projectSidebarWidth: projectSidebarWidth
            )
        )
        return NSRect(
            x: contentRect.maxX - width,
            y: contentRect.minY,
            width: width,
            height: contentRect.height
        )
    }

    /// Pure eligibility + hit-test rule. The visible square is exact, while a
    /// narrow trailing runway lets the cursor reach the literal window edge
    /// without falling out of it. Every eligible Session drag gets this
    /// destination: at the far-right edge of an empty project sidebar it
    /// replaces Split Right. An open panel uses its real panes for insertion.
    nonisolated static func isProjectSidebarPinTarget(
        at point: NSPoint,
        contentRect: NSRect,
        trailingEdgeX: CGFloat,
        projectSidebarIsOpen: Bool,
        canPin: Bool,
        projectSidebarWidth: CGFloat = Theme.sidebarDefaultWidth,
        targetWasExpanded: Bool = false
    ) -> Bool {
        guard !projectSidebarIsOpen, canPin,
              contentRect.width >= projectSidebarPinTargetSize
                  + projectSidebarPinTargetTrailingInset * 2,
              contentRect.height >= projectSidebarPinTargetSize
                  + projectSidebarPinTargetTopInset * 2
        else { return false }
        guard point.y >= contentRect.minY, point.y < contentRect.maxY,
              point.x >= contentRect.minX,
              point.x <= trailingEdgeX + projectSidebarPinEdgeOvershoot
        else { return false }
        let target = projectSidebarPinTargetRect(in: contentRect)
        let runwayMinX = contentRect.maxX - projectSidebarPinEdgeRunwayWidth
        let runway = NSRect(
            x: runwayMinX,
            y: contentRect.minY,
            width: max(
                0,
                trailingEdgeX + projectSidebarPinEdgeOvershoot - runwayMinX
            ),
            height: contentRect.height
        )
        let expandedTarget = projectSidebarPinPreviewRect(
            in: contentRect,
            projectSidebarWidth: projectSidebarWidth
        )
        return target.contains(point)
            || runway.contains(point)
            || (targetWasExpanded && expandedTarget.contains(point))
    }

    /// The Pin indicator, not the detached card or target footprint, leans
    /// toward the pointer. Input is AppKit window space (y-up); output is
    /// SwiftUI offset space (y-down).
    nonisolated static func projectSidebarPinSquareLean(
        cursor: NSPoint,
        targetRect: NSRect
    ) -> CGSize {
        let deltaX = cursor.x - targetRect.midX
        let deltaY = targetRect.midY - cursor.y
        let distance = hypot(deltaX, deltaY)
        guard distance > 0 else { return .zero }
        let scale = min(0.18, projectSidebarPinSquareLeanMax / distance)
        return CGSize(
            width: deltaX * scale,
            height: deltaY * scale
        )
    }

    /// Exposes the first right-panel slot while an eligible pane title is
    /// dragged over the square. Once the panel exists, `updatePanelReorder`
    /// takes over using the actual stacked pane frames.
    private func updateProjectSidebarPinTarget(
        insideSidebar: Bool,
        in view: NSView
    ) {
        let targetWasExpanded = projectSidebarPinDropRect != nil
        var available = false
        var targetRect: NSRect?
        var targetLeanRect: NSRect?
        if !insideSidebar,
           let store,
           let drag = dragState?.sessionDrag,
           let parent = view.window {
            let paneRects = paneDropTargets.values.compactMap { record -> NSRect? in
                guard let paneView = record.view, paneView.window === parent
                else { return nil }
                return paneView.convert(paneView.bounds, to: nil)
            }
            if let first = paneRects.first {
                let contentRect = paneRects.dropFirst().reduce(first) {
                    $0.union($1)
                }
                available = store.projectSidebarSessions.isEmpty
                    && store.canMoveSessionToProjectSidebar(drag.sessionID)
                    && contentRect.width >= Self.projectSidebarPinTargetSize
                        + Self.projectSidebarPinTargetTrailingInset * 2
                    && contentRect.height >= Self.projectSidebarPinTargetSize
                        + Self.projectSidebarPinTargetTopInset * 2
                if Self.isProjectSidebarPinTarget(
                    at: lastWindowLocation,
                    contentRect: contentRect,
                    trailingEdgeX: parent.contentLayoutRect.maxX,
                    projectSidebarIsOpen: !store.projectSidebarSessions.isEmpty,
                    canPin: store.canMoveSessionToProjectSidebar(drag.sessionID),
                    projectSidebarWidth: projectSidebarWidth,
                    targetWasExpanded: targetWasExpanded
                ) {
                    targetRect = Self.projectSidebarPinTargetRect(in: contentRect)
                    targetLeanRect = Self.projectSidebarPinPreviewRect(
                        in: contentRect,
                        projectSidebarWidth: projectSidebarWidth
                    )
                }
            }
        }
        projectSidebarPinDropState.set(
            available: available,
            targeted: targetRect != nil,
            cursorLean: targetLeanRect.map {
                Self.projectSidebarPinSquareLean(
                    cursor: lastWindowLocation,
                    targetRect: $0
                )
            } ?? .zero,
            previewPaneWidth: Self.projectSidebarPinPreviewPaneWidth(
                projectSidebarWidth: projectSidebarWidth
            )
        )
        guard targetRect != projectSidebarPinDropRect else { return }
        projectSidebarPinDropRect = targetRect
    }

    private func updateTerminalDropZone(insideSidebar: Bool, in view: NSView) {
        var zone: PaneDropTarget?
        var squareLean: CGFloat = 0
        if !insideSidebar,
           let store,
           let drag = dragState?.sessionDrag,
           // A hovered panel target owns the drop; the main content rect
           // reaches under the panel, so its right group-edge band must not
           // light up at the same time.
           panelReorderTargetID == nil,
           panelInsertTarget == nil,
           projectSidebarPinDropRect == nil,
           store.canSplitTerminal(with: drag.sessionID),
           let parent = view.window {
            let sidebarRightX = view.convert(
                NSPoint(x: view.bounds.maxX, y: 0), to: nil
            ).x
            let content = parent.contentLayoutRect
            let contentWidth = content.maxX - sidebarRightX
            if contentWidth > 240 {
                zone = Self.dropTarget(
                    at: lastWindowLocation,
                    contentRect: NSRect(
                        x: sidebarRightX,
                        y: content.minY,
                        width: contentWidth,
                        height: content.height
                    ),
                    panes: paneDropTargets.values.compactMap { record in
                        guard let paneView = record.view,
                              paneView.window === parent
                        else { return nil }
                        return (
                            paneID: record.paneID,
                            isSolo: record.isSolo,
                            rect: paneView.convert(paneView.bounds, to: nil)
                        )
                    }
                )
                if case let .groupEdge(edge) = zone, edge == .left || edge == .right {
                    // The band reaches toward the chip as the card closes in.
                    let center = edge == .left
                        ? sidebarRightX + Self.groupEdgeBandWidth
                        : content.maxX - Self.groupEdgeBandWidth
                    squareLean = Self.dropZoneSquareLean(
                        cursorX: lastWindowLocation.x, zoneCenterX: center
                    )
                }
            }
        }
        if zone != hoveredDropZone {
            hoveredDropZone = zone
            store?.setTerminalPaneDropTarget(zone)
        }
        dropZoneMagnet.setOffsetX(squareLean)
    }

    /// Pure drop-target resolution in AppKit window coordinates (y-up: a
    /// larger y is visually higher, so PaneEdge.up maps to the maxY side).
    /// The outer band of the content area targets the whole group's edge;
    /// inside a registered pane, the nearest of its four edges wins
    /// (Ghostty's triangular zones). Short panes refuse up/down and fall
    /// back to their horizontal half.
    nonisolated static func dropTarget(
        at point: NSPoint,
        contentRect: NSRect,
        panes: [(paneID: String, isSolo: Bool, rect: NSRect)]
    ) -> PaneDropTarget? {
        guard contentRect.contains(point) else { return nil }

        let band = groupEdgeBandWidth
        if point.x - contentRect.minX < band { return .groupEdge(.left) }
        if contentRect.maxX - point.x < band { return .groupEdge(.right) }
        if contentRect.maxY - point.y < band { return .groupEdge(.up) }
        if point.y - contentRect.minY < band { return .groupEdge(.down) }

        for pane in panes where pane.rect.contains(point) {
            let distanceLeft = point.x - pane.rect.minX
            let distanceRight = pane.rect.maxX - point.x
            let distanceUp = pane.rect.maxY - point.y
            let distanceDown = point.y - pane.rect.minY
            var edge: PaneEdge
            if min(distanceLeft, distanceRight) <= min(distanceUp, distanceDown) {
                edge = distanceLeft <= distanceRight ? .left : .right
            } else {
                edge = distanceUp <= distanceDown ? .up : .down
            }
            if (edge == .up || edge == .down),
               pane.rect.height < minimumVerticalSplitTargetHeight {
                edge = distanceLeft <= distanceRight ? .left : .right
            }
            return pane.isSolo
                ? .groupEdge(edge)
                : .pane(paneID: pane.paneID, edge: edge)
        }
        return nil
    }

    private func clearDropZone() {
        dropZoneMagnet.setOffsetX(0)
        projectSidebarPinDropRect = nil
        projectSidebarPinDropState.set(available: false, targeted: false)
        guard hoveredDropZone != nil else { return }
        hoveredDropZone = nil
        store?.setTerminalPaneDropTarget(nil)
    }

    /// The card's magnetic lean toward the nearest split drop zone: same
    /// eligibility and geometry as `updateTerminalDropZone`, applied as a
    /// horizontal shift on the card window while it rides the content area.
    private func dropZoneMagnetShift(in view: NSView) -> CGFloat {
        // The card magnet only pulls toward the left/right group-edge bands;
        // pane-edge targets get their feedback from the in-pane highlight.
        guard case let .groupEdge(edge) = hoveredDropZone,
              edge == .left || edge == .right,
              let parent = view.window
        else { return 0 }
        let sidebarRightX = view.convert(
            NSPoint(x: view.bounds.maxX, y: 0), to: nil
        ).x
        let content = parent.contentLayoutRect
        let center = edge == .left
            ? sidebarRightX + Self.groupEdgeBandWidth
            : content.maxX - Self.groupEdgeBandWidth
        return Self.dropZoneMagnetShift(
            cursorX: lastWindowLocation.x,
            zoneCenterX: center,
            attractionRadius: Self.groupEdgeBandWidth * 3
        )
    }

    /// The card's pull toward a hovered SORT target (a pane-title swap or a
    /// right-panel reorder member). The top-right Pin square is intentionally
    /// excluded: it leans toward the cursor instead, preserving cursor lock.
    private func sortTargetMagnetShift() -> NSPoint {
        guard projectSidebarPinDropRect == nil else { return .zero }
        var rect: NSRect?
        if let context = paneDragContext,
           let target = context.lastTargetPaneID,
           let view = paneDropTargets[target]?.view ?? paneSources[target]?.view,
           view.window != nil {
            rect = view.convert(view.bounds, to: nil)
        } else if let target = panelReorderTargetID ?? panelInsertTarget?.sessionID,
                  let view = panelPaneSources[target]?.view,
                  view.window != nil {
            rect = view.convert(view.bounds, to: nil)
        }
        guard let rect else { return .zero }
        let attractionRadius = max(rect.width / 2, 1)
        return NSPoint(
            x: Self.dropZoneMagnetShift(
                cursorX: lastWindowLocation.x,
                zoneCenterX: rect.midX,
                attractionRadius: attractionRadius
            ),
            y: Self.dropZoneMagnetShift(
                cursorX: lastWindowLocation.y,
                zoneCenterX: rect.midY,
                attractionRadius: max(rect.height / 2, 1)
            )
        )
    }

    /// Pure magnet curve: the shift grows as the cursor closes on the zone
    /// center, then eases back to 0 AT the center (a card centered in the
    /// zone needs no lean, and a shift that survives the center would flip
    /// sign there — a visible jump). Zero at/beyond the attraction radius,
    /// capped so the lean stays subtle.
    nonisolated static func dropZoneMagnetShift(
        cursorX: CGFloat, zoneCenterX: CGFloat, attractionRadius: CGFloat
    ) -> CGFloat {
        guard attractionRadius > 0 else { return 0 }
        let distance = zoneCenterX - cursorX
        let closeness = max(0, 1 - abs(distance) / attractionRadius)
        let shift = distance * closeness * closeness * 0.25
        return min(max(shift, -dropZoneMagnetMaxShift), dropZoneMagnetMaxShift)
    }

    nonisolated static let dropZoneMagnetMaxShift: CGFloat = 18

    /// The square's mirror lean toward the chip: proportional to the chip's
    /// distance from the zone center (toward the chip), zero once centered,
    /// capped tighter than the card's own pull so the two never fight.
    nonisolated static func dropZoneSquareLean(
        cursorX: CGFloat, zoneCenterX: CGFloat
    ) -> CGFloat {
        let shift = (cursorX - zoneCenterX) * 0.18
        return min(max(shift, -dropZoneSquareLeanMax), dropZoneSquareLeanMax)
    }

    nonisolated static let dropZoneSquareLeanMax: CGFloat = 9

    static func pinnedBoundary(for kind: RowKind) -> PinnedDragBoundary? {
        switch kind {
        case let .session(projectID, pinned, _):
            return pinned ? .partition(parentID: projectID) : nil
        case let .folderItem(parentID, pinned, _):
            return pinned ? .partition(parentID: parentID) : nil
        case .project, .projectShell, .groupShell:
            return nil
        }
    }

    /// Resolve a sidebar row under a pinned drag. This is deliberately pure:
    /// the same rule drives the visual gap and the final accepted/refused
    /// drop, so a pinned row can never preview one destination and land in
    /// another partition. Both pinned row kinds (Session or child group)
    /// share the parent's one mixed partition: any pinned sibling — either
    /// kind — is a reorder anchor; everything outside the partition denies.
    static func pinnedSidebarHitAction(
        boundary: PinnedDragBoundary,
        sourceID: String,
        targetID: String,
        targetKind: RowKind,
        firstReorderID: String?
    ) -> PinnedSidebarHitAction {
        let parentID: String
        switch boundary {
        case let .partition(id): parentID = id
        }
        switch targetKind {
        case let .session(targetProjectID, pinned, _):
            if targetID == sourceID {
                return .reorder(anchorID: sourceID)
            }
            // A dragged pinned GROUP's own member Sessions are still its
            // home, never an attempted move out of the pinned partition.
            if targetProjectID == sourceID { return .allow }
            return targetProjectID == parentID && pinned
                ? .reorder(anchorID: targetID)
                : .deny
        case let .folderItem(targetParentID, pinned, _):
            if targetID == sourceID {
                return .reorder(anchorID: sourceID)
            }
            // Rows nested inside an expanded dragged group are still its
            // home too.
            if targetParentID == sourceID { return .allow }
            // A Session filed in a child group sees that group's own
            // folder header. It is the top-of-pinned-partition anchor, not
            // a request to move the Session out to the parent.
            if targetID == parentID {
                return .reorder(anchorID: firstReorderID ?? sourceID)
            }
            return targetParentID == parentID && pinned
                ? .reorder(anchorID: targetID)
                : .deny
        case .project:
            return targetID == parentID
                ? .reorder(anchorID: firstReorderID ?? sourceID)
                : .deny
        case let .groupShell(targetProjectID):
            // The dragged group's own wash, or blank space in the owning
            // parent group's shell, retains the current gap.
            return targetProjectID == parentID || targetProjectID == sourceID
                ? .allow
                : .deny
        case let .projectShell(targetProjectID):
            // Blank space in the owning project's shell retains the
            // current pinned gap. A semantic row inside it wins
            // hit-testing and is checked more narrowly above.
            return targetProjectID == parentID ? .allow : .deny
        }
    }

    /// Content-area policy is intentionally independent of sidebar pinning:
    /// a real split target accepts a pinned Session because panes are
    /// Controller presentation state, not Host-side filing. A pane-title drag
    /// also remains valid while swapping within its existing pane group.
    nonisolated static func pinnedContentDropIsDenied(
        hasPaneDropTarget: Bool,
        isPaneTitleDrag: Bool
    ) -> Bool {
        !hasPaneDropTarget && !isPaneTitleDrag
    }

    /// One dictionary scan per mouse move; store/dragState calls only fire
    /// when the hovered row identity (or the insertion gap) actually changes.
    private func hitTest(_ content: CGPoint, insideSidebar: Bool = true) {
        guard let store, let dragState else { return }
        if let draggedProjectID = dragState.projectID {
            // Do NOT live-rebuild the full tree here. Expanded projects move
            // large subtrees; their animated geometry used to change the row
            // under the same cursor, bounce the preview back, and saturate the
            // main thread. Track only a stable target and apply one preview at
            // mouse-up, immediately before the existing single commit.
            let hit = projectBlock(at: content)
            projectDropTargetID = Self.retainedProjectDropTarget(
                current: projectDropTargetID,
                hit: hit?.id,
                draggedID: draggedProjectID
            )
            if let hit, hit.id != draggedProjectID {
                projectDropBelow = hit.below
            }
            dragState.setProjectInsertion(projectDropTargetID.map {
                SidebarDragState.ProjectInsertion(
                    projectID: $0, below: projectDropBelow
                )
            })
            return
        }
        guard let drag = dragState.sessionDrag else { return }
        if let boundary = pinnedDragBoundary {
            clearHoveredTarget()
            updateSpringOpen(candidate: nil)

            // Crossing into the content area is allowed only when a real pane
            // drop zone accepts the Session. A pane-title drag is already in a
            // pane and uses that area to swap pane positions, so it remains a
            // valid presentation-only drag too.
            guard insideSidebar else {
                clearInsertion()
                clearReorderTarget()
                pinnedDropDenied = Self.pinnedContentDropIsDenied(
                    hasPaneDropTarget: hoveredDropZone != nil,
                    isPaneTitleDrag: paneDragContext != nil
                )
                return
            }

            guard let (id, record) = row(at: content) else {
                // Tiny stack gaps and blank space in the current shell retain
                // the last valid pinned reorder gap, like ordinary drags.
                pinnedDropDenied = false
        worktreeDropDenied = false
                return
            }
            let action = Self.pinnedSidebarHitAction(
                boundary: boundary,
                sourceID: drag.sessionID,
                targetID: id,
                targetKind: record.kind,
                firstReorderID: dragState.sessionReorderFirstID
            )
            var insertion: SidebarDragState.SessionInsertion?
            switch action {
            case .allow:
                pinnedDropDenied = false
        worktreeDropDenied = false
            case let .reorder(anchorID):
                pinnedDropDenied = false
        worktreeDropDenied = false
                if paneDragContext != nil,
                   case let .session(projectID, pinned, _) = record.kind,
                   projectID == drag.projectID,
                   pinned == drag.pinned,
                   id != drag.sessionID {
                    // A pane member has no visible source slot/map. Detaching
                    // it back into its own pinned section uses the ordinary
                    // explicit insertion gap instead.
                    insertion = SidebarDragState.SessionInsertion(
                        projectID: projectID,
                        anchorID: id,
                        below: content.y > record.frame.midY
                    )
                } else if anchorID != reorderAnchorID,
                          let index = dragState.sessionReorderIndexByID[anchorID] {
                    reorderAnchorID = anchorID
                    dragState.setSessionReorderTarget(index)
                }
            case .deny:
                pinnedDropDenied = true
                clearReorderTarget()
            }
            if insertion != hoveredInsertion {
                hoveredInsertion = insertion
                dragState.setSessionInsertion(insertion)
            }
            return
        }
        pinnedDropDenied = false
        worktreeDropDenied = false
        var newTarget: String?
        var newInsertion: SidebarDragState.SessionInsertion?
        var newReorderAnchor: String?
        var springCandidate: String?
        if let (id, record) = row(at: content) {
            // Checkout boundary first: a row owned by a project in another
            // git checkout than the dragged Session's home can never accept
            // it, never previews a gap, and never spring-opens. The origin
            // section's own sibling folders stay positional (below).
            let hoveredOwnerProjectID: String?
            switch record.kind {
            case let .session(projectID, _, _):
                hoveredOwnerProjectID = projectID
            case let .folderItem(parentID, _, _):
                hoveredOwnerProjectID = parentID == drag.projectID ? nil : id
            case .project:
                hoveredOwnerProjectID = id
            case let .projectShell(projectID), let .groupShell(projectID):
                hoveredOwnerProjectID = projectID
            }
            if let hoveredOwnerProjectID,
               hoveredOwnerProjectID != drag.projectID,
               store.sessionMoveCrossesCheckout(
                   drag.sessionID, hoveringProjectID: hoveredOwnerProjectID
               ) {
                worktreeDropDenied = true
            }
            switch record.kind {
            case let .session(projectID, pinned, _):
                if projectID == drag.projectID, paneDragContext == nil {
                    // Same-section rows move the TRANSFORM gap — one small
                    // published index; zero store work, no tree rebuild.
                    // Pinned and regular rows never mix (same rule as
                    // before); the dragged row's own slot is "gap at home".
                    if pinned == drag.pinned {
                        newReorderAnchor = id
                    }
                } else if projectID == drag.projectID {
                    // Grouped pane members have no visible source row while
                    // the multi-pane view is open. Treat same-group rows as
                    // explicit gaps;
                    // on drop the pane detaches first, then lands exactly at
                    // this position.
                    if pinned == drag.pinned, id != drag.sessionID {
                        newInsertion = SidebarDragState.SessionInsertion(
                            projectID: projectID,
                            anchorID: id,
                            below: content.y > record.frame.midY
                        )
                    }
                } else if store.canMoveSession(drag.sessionID, toProjectID: projectID) {
                    // A session row inside ANOTHER group that can accept the
                    // drag: the group is the move target, and — for the
                    // matching section — the gap above/below the hovered row
                    // is a positional insertion point. A date-sorted target
                    // offers it too: committing there flips the list to
                    // custom order (same rule as reordering within one), so
                    // the dropped position sticks. Lists whose group can't
                    // accept never get here, so they never preview.
                    newTarget = projectID
                    if pinned == drag.pinned {
                        newInsertion = SidebarDragState.SessionInsertion(
                            projectID: projectID,
                            anchorID: id,
                            below: content.y > record.frame.midY
                        )
                    }
                }
            case let .folderItem(parentID, _, _):
                if parentID == drag.projectID, !drag.pinned, paneDragContext == nil {
                    if card?.folderName != nil {
                        // A dragged FOLDER reorders against its sibling
                        // folders like any other slot in the mixed list.
                        newReorderAnchor = id
                    } else if !store.canMoveSession(drag.sessionID, toProjectID: id) {
                        // A sibling folder that can't accept the session
                        // (a worktree): purely positional — the gap opens
                        // at its slot, exactly like a session row.
                        newReorderAnchor = id
                    } else if Self.folderHoverIsPositional(
                        y: content.y,
                        rowMinY: record.frame.minY,
                        rowHeight: record.frame.height
                    ) {
                        // Top/bottom band of a sibling group: place the
                        // session ABOVE/BELOW the group (essential when
                        // the group sits first or last in the list).
                        newReorderAnchor = id
                    } else {
                        // The middle files INTO the group (highlight +
                        // spring-open + drop). The group must stay put
                        // under the cursor here, so this zone never moves
                        // the gap.
                        newTarget = id
                        springCandidate = id
                    }
                } else if id == drag.projectID, paneDragContext == nil {
                    // The origin GROUP's own header, dragging one of its
                    // members: the gap goes to the top of its list.
                    newReorderAnchor = dragState.sessionReorderFirstID
                } else {
                    if store.canMoveSession(drag.sessionID, toProjectID: id) {
                        newTarget = id
                    }
                    // Spring-loading on any collapsed foreign folder with
                    // contents — also non-targets whose children may
                    // accept the drop once revealed.
                    springCandidate = id
                }
            case .project:
                // Same gating as the old drop delegates: a target that
                // lights up always moves (canMoveSession mirrors the verb's
                // guards — worktrees, foreign projects, the current
                // location, and remote/scoped-workspace sessions never
                // highlight).
                if id == drag.projectID, paneDragContext == nil {
                    // The origin project's own header: the gap goes to the
                    // TOP of the origin section — the only way to place a
                    // session above a group/folder sitting first in the
                    // list.
                    newReorderAnchor = dragState.sessionReorderFirstID
                } else if store.canMoveSession(drag.sessionID, toProjectID: id) {
                    newTarget = id
                }
                // Spring-loading works on ANY collapsed row with contents —
                // also non-targets (e.g. a worktree folder) whose children
                // may accept the drop once revealed.
                springCandidate = id
            case let .projectShell(projectID):
                if store.canMoveSession(drag.sessionID, toProjectID: projectID) {
                    newTarget = projectID
                }
                springCandidate = projectID
            case let .groupShell(projectID):
                if store.canMoveSession(drag.sessionID, toProjectID: projectID) {
                    newTarget = projectID
                }
                springCandidate = projectID
            }
            if worktreeDropDenied {
                // Defense in depth against the per-case guards above: a
                // cross-checkout row offers no target, no gap, no spring.
                newTarget = nil
                newInsertion = nil
                newReorderAnchor = nil
                springCandidate = nil
            }
        }
        if newTarget != hoveredTargetProjectID {
            if let old = hoveredTargetProjectID {
                dragState.setSessionDropTarget(old, hovering: false)
            }
            if let new = newTarget {
                dragState.setSessionDropTarget(new, hovering: true)
            }
            hoveredTargetProjectID = newTarget
        }
        if newInsertion != hoveredInsertion {
            hoveredInsertion = newInsertion
            dragState.setSessionInsertion(newInsertion)
        }
        updateSpringOpen(candidate: springCandidate)
        // A nil anchor RETAINS the current gap (hovering the origin
        // project's header or the 1pt stack gaps must not flutter the slot
        // closed) — the same retention rule `projectDropTargetID` uses.
        // Only leaving the sidebar (trackCursor) or landing clears it.
        // Anchors must resolve in the frozen map: a drag with no map (an
        // expanded folder as the source) or a row that appeared mid-drag
        // must not commit a reorder that no gap ever showed.
        if let anchor = newReorderAnchor, anchor != reorderAnchorID,
           let index = dragState.sessionReorderIndexByID[anchor] {
            reorderAnchorID = anchor
            dragState.setSessionReorderTarget(index)
        }
    }

    /// Animated close of the same-section transform gap: published while
    /// the drag is still active, so the rows spring back home.
    private func clearReorderTarget() {
        guard reorderAnchorID != nil else { return }
        reorderAnchorID = nil
        dragState?.setSessionReorderTarget(nil)
    }

    private func endDrag(in view: NSView) {
        stopAutoScroll()
        cancelSpringOpen()
        guard let store, let dragState else {
            generation += 1
            forceClearCard()
            return
        }
        if dragState.projectID != nil {
            // Project drags intentionally do no tree work on the 60Hz hot
            // path. An accepted release resolves the gap, final order and
            // detached card in ONE unanimated transaction. Springing the card
            // toward a row while the tree and insertion gap also animated made
            // the drop look like several competing landings.
            let content = view.convert(lastWindowLocation, from: nil)
            let inside = content.x >= 0 && content.x <= view.bounds.width
            if inside,
               let draggedID = dragState.projectID,
               let targetID = projectDropTargetID {
                snapProjectDrop(
                    draggedID: draggedID,
                    targetID: targetID,
                    below: projectDropBelow,
                    store: store,
                    dragState: dragState
                )
            } else {
                settle(commit: false)
            }
            return
        }
        guard let drag = dragState.sessionDrag else {
            generation += 1
            forceClearCard()
            return
        }
        // Right-panel drop: the hovered pane carried the preview. A member
        // commits its reorder (full-frame preview); any other pinnable
        // session pins into the panel at the previewed half's position. One
        // durable write, here (scope-routed).
        if let target = panelReorderTargetID,
           store.sessionIsInProjectSidebar(drag.sessionID) {
            clearDropZone()
            store.cancelSessionReorder(projectID: drag.projectID)
            store.commitProjectSidebarReorder(
                draggedID: drag.sessionID,
                over: target,
                groupID: drag.projectID
            )
            store.setProjectSidebarReorderPreview(nil)
            panelReorderTargetID = nil
            dragState.end()
            fadeOutCardInPlace()
            return
        }
        if let insert = panelInsertTarget,
           store.canMoveSessionToProjectSidebar(drag.sessionID) {
            clearDropZone()
            store.cancelSessionReorder(projectID: drag.projectID)
            let members = store.projectSidebarSessions.map(\.id)
            let index = members.firstIndex(of: insert.sessionID)
                .map { $0 + (insert.below ? 1 : 0) }
            store.pinSessionToProjectSidebar(drag.sessionID, at: index)
            store.setProjectSidebarInsertPreview(nil)
            panelInsertTarget = nil
            dragState.end()
            fadeOutCardInPlace()
            return
        }
        if projectSidebarPinDropRect != nil,
           store.canMoveSessionToProjectSidebar(drag.sessionID) {
            clearDropZone()
            store.cancelSessionReorder(projectID: drag.projectID)
            store.pinSessionToProjectSidebar(drag.sessionID, at: nil)
            dragState.end()
            fadeOutCardInPlace()
            return
        }
        if let zone = hoveredDropZone {
            settleIntoSplit(zone: zone, drag: drag, store: store, dragState: dragState)
            return
        }
        if pinnedDropDenied {
            // The drag was useful up to this point (pinned reordering and pane
            // placement both need a real lift), but this release crosses the
            // source's pinned sidebar boundary. Return it home, then give the
            // same small "no" shake that used to happen prematurely at lift.
            settle(commit: false, deniedRowID: drag.sessionID)
            return
        }
        if worktreeDropDenied {
            // Released over another git checkout: the Session's shell runs
            // in its own worktree, so filing it elsewhere would lie about
            // where it runs. Return it home with the same "no" shake.
            settle(commit: false, deniedRowID: drag.sessionID)
            return
        }
        if let insertion = hoveredInsertion {
            releaseDraggedPaneToSidebar(store: store)
            settleIntoList(insertion: insertion, drag: drag, store: store, dragState: dragState)
            return
        }
        if let target = hoveredTargetProjectID {
            releaseDraggedPaneToSidebar(store: store)
            settleIntoGroup(target: target, drag: drag, store: store, dragState: dragState)
            return
        }
        // Anywhere over the list lands the drag; releasing outside the
        // sidebar cancels.
        let content = view.convert(lastWindowLocation, from: nil)
        let inside = content.x >= 0 && content.x <= view.bounds.width
        if let context = paneDragContext {
            // Pane-title swap: the hovered sibling carried the preview
            // highlight; the tree swaps exactly once, on this drop.
            if let groupID = context.groupID,
               let target = context.lastTargetPaneID {
                store.swapTerminalPanes(
                    groupID: groupID,
                    context.paneID,
                    with: target
                )
                store.setPaneSwapPreview(nil)
                dragState.end()
                fadeOutCardInPlace()
            } else if inside {
                releaseDraggedPaneToSidebar(store: store)
                settle(commit: true)
            } else {
                dragState.end()
                fadeOutCardInPlace()
            }
            return
        }
        // Same-section positional drop: the ONE moment the tree actually
        // reorders. The card settles into the open gap (the anchor row's
        // static slot), then the store preview + commit apply in a single
        // unanimated transaction — pixel-identical to the transform gap the
        // rows already show, so nothing visibly moves at the swap.
        if inside, let anchor = reorderAnchorID, anchor != drag.sessionID {
            settleIntoReorder(
                anchorID: anchor, drag: drag, store: store, dragState: dragState
            )
            return
        }
        settle(commit: inside)
    }

    /// Accepted same-section reorder: spring the card into the gap, then
    /// apply the final order exactly once. `previewSessionMove` (anchored on
    /// row ids against the CURRENT rendered list, so a mid-drag rescan can't
    /// misplace the drop) recreates the visual order in real layout, and
    /// `finish()` persists it through the existing commit closure. All in
    /// one animation-disabled transaction with the transform-gap reset, so
    /// the layout swap is invisible.
    private func settleIntoReorder(
        anchorID: String,
        drag: SidebarDragState.SessionDrag,
        store: UnpeelStore,
        dragState: SidebarDragState
    ) {
        phase = .settling
        stopDragTimer()
        stopAutoScroll()
        cancelSpringOpen()
        clearHoveredTarget()
        clearInsertion()
        clearDropZone()
        generation += 1
        let gen = generation
        let land = { [weak self] in
            guard let self else { return }
            var transaction = Transaction()
            transaction.disablesAnimations = true
            withTransaction(transaction) {
                if drag.pinned {
                    store.previewPinnedSessionMove(
                        projectID: drag.projectID,
                        draggedID: drag.sessionID,
                        over: anchorID
                    )
                } else {
                    store.previewSessionMove(
                        projectID: drag.projectID,
                        draggedID: drag.sessionID,
                        over: anchorID
                    )
                }
                dragState.finish()
                self.forceClearCard()
            }
        }
        if SidebarMotion.reduceMotion || cardWindow == nil {
            land()
            return
        }
        if let state = cardState {
            withAnimation(Self.settleAnimation) { state.lifted = false }
        }
        // The gap sits exactly over the anchor row's static frame (the
        // in-between rows shifted one slot toward the origin), so the
        // anchor IS the card's landing rect.
        if let window = cardWindow, let dest = cardWindowOrigin(overRow: anchorID) {
            NSAnimationContext.runAnimationGroup { context in
                context.duration = Self.settleDelay
                context.timingFunction = Self.settleTimingFunction()
                window.animator().setFrame(
                    NSRect(origin: dest, size: window.frame.size), display: true
                )
            }
        }
        DispatchQueue.main.asyncAfter(deadline: .now() + Self.settleDelay) { [weak self] in
            guard let self, self.generation == gen else { return }
            land()
        }
    }

    /// A grouped Session is hidden under one representative row. Accepting a
    /// sidebar drop first detaches that pane, then the existing reorder/move
    /// verb owns its exact destination. Representative panes are symmetric:
    /// detaching one promotes another member when the group survives.
    private func releaseDraggedPaneToSidebar(store: UnpeelStore) {
        guard let context = paneDragContext, context.groupID != nil else { return }
        store.detachTerminalPane(context.paneID)
    }

    /// Accepted project reorder: visually snap directly from the live gap to
    /// the committed order. The preview still exists briefly because the
    /// existing commit path derives its durable sibling order from that exact
    /// displayed projection, but every state mutation is in one transaction
    /// with animations disabled, so there is no intermediate frame.
    private func snapProjectDrop(
        draggedID: String,
        targetID: String,
        below: Bool,
        store: UnpeelStore,
        dragState: SidebarDragState
    ) {
        phase = .settling
        stopDragTimer()
        stopAutoScroll()
        cancelSpringOpen()
        generation += 1

        var transaction = Transaction()
        transaction.disablesAnimations = true
        withTransaction(transaction) {
            clearHoveredTarget()
            clearInsertion()
            clearDropZone()
            dragState.setLiftedSlotCollapsed(false)
            store.previewProjectMove(
                draggedID: draggedID,
                targetID: targetID,
                below: below
            )
            dragState.finish()
            forceClearCard()
        }
    }

    /// Card settles into its current slot (spring), THEN the drag state
    /// commits or cancels — so the source row never pops back while the card
    /// is still in flight. On cancel the list slides home right after.
    private func settle(commit: Bool, deniedRowID: String? = nil) {
        guard let dragState else { return }
        let rowID = dragState.sessionDrag?.sessionID ?? dragState.projectID
        guard let rowID else { return }
        phase = .settling
        stopDragTimer()
        stopAutoScroll()
        cancelSpringOpen()
        clearHoveredTarget()
        clearInsertion()
        clearDropZone()
        // The same-section gap springs closed while the card flies home —
        // the drag state is still active, so the rows animate back.
        clearReorderTarget()
        // Reopen a collapsed origin slot so the card has a real gap to
        // spring home into.
        dragState.setLiftedSlotCollapsed(false)
        generation += 1
        let gen = generation
        if SidebarMotion.reduceMotion || cardWindow == nil {
            commit ? dragState.finish() : dragState.end()
            forceClearCard()
            if let deniedRowID { shakeDeniedRow(deniedRowID) }
            return
        }
        if let state = cardState {
            withAnimation(Self.settleAnimation) { state.lifted = false }
        }
        if let window = cardWindow, let dest = cardWindowOrigin(overRow: rowID) {
            NSAnimationContext.runAnimationGroup { context in
                context.duration = Self.settleDelay
                context.timingFunction = Self.settleTimingFunction()
                window.animator().setFrame(
                    NSRect(origin: dest, size: window.frame.size), display: true
                )
            }
        }
        DispatchQueue.main.asyncAfter(deadline: .now() + Self.settleDelay) { [weak self] in
            guard let self, self.generation == gen else { return }
            commit ? self.dragState?.finish() : self.dragState?.end()
            self.forceClearCard()
            if let deniedRowID { self.shakeDeniedRow(deniedRowID) }
        }
    }

    private func shakeDeniedRow(_ rowID: String) {
        guard !SidebarMotion.reduceMotion, let dragState else { return }
        withAnimation(.linear(duration: 0.45)) {
            dragState.bumpDeniedShake(rowID)
        }
    }

    /// Drop on a terminal-edge split zone: the Session stays exactly where
    /// it is in the sidebar. This is a view action, never a Session move; the
    /// origin reorder preview cancels and the window records the pane.
    /// The card fades out in place; the new pane's reveal is the feedback.
    private func settleIntoSplit(
        zone: PaneDropTarget,
        drag: SidebarDragState.SessionDrag,
        store: UnpeelStore,
        dragState: SidebarDragState
    ) {
        clearDropZone()
        store.cancelSessionReorder(projectID: drag.projectID)
        store.splitTerminal(with: drag.sessionID, near: zone)
        dragState.end()
        fadeOutCardInPlace()
    }

    /// Card exit with no destination row (terminal-edge drop): quick fade +
    /// slight shrink where it stands.
    private func fadeOutCardInPlace() {
        phase = .settling
        stopDragTimer()
        clearHoveredTarget()
        clearInsertion()
        generation += 1
        let gen = generation
        guard !SidebarMotion.reduceMotion, let window = cardWindow else {
            forceClearCard()
            return
        }
        if let state = cardState {
            withAnimation(.easeOut(duration: 0.14)) { state.lifted = false }
        }
        NSAnimationContext.runAnimationGroup { context in
            context.duration = 0.14
            context.timingFunction = CAMediaTimingFunction(name: .easeOut)
            window.animator().alphaValue = 0
        }
        DispatchQueue.main.asyncAfter(deadline: .now() + 0.16) { [weak self] in
            guard let self, self.generation == gen else { return }
            self.forceClearCard()
        }
    }

    /// Drop on a highlighted group/root row: the SAME verbs the system drop
    /// performed (cancel origin preview, `moveSession`, clear drag state);
    /// the card fades into the target row.
    private func settleIntoGroup(
        target: String,
        drag: SidebarDragState.SessionDrag,
        store: UnpeelStore,
        dragState: SidebarDragState
    ) {
        clearReorderTarget()
        store.cancelSessionReorder(projectID: drag.projectID)
        store.moveSession(drag.sessionID, toProjectID: target)
        dragState.end()
        fadeOutCard(towardRow: target)
    }

    /// Drop into the gap between two rows of ANOTHER group's session list:
    /// composed from the existing verbs — `moveSession` files the session
    /// into the target group first (shared marker + synchronous rescan), then
    /// `setSessionOrder`/`setPinnedOrder` pins the visible position within
    /// the section it landed in, in the same commit sequence, so the final
    /// order sticks.
    private func settleIntoList(
        insertion: SidebarDragState.SessionInsertion,
        drag: SidebarDragState.SessionDrag,
        store: UnpeelStore,
        dragState: SidebarDragState
    ) {
        clearReorderTarget()
        store.cancelSessionReorder(projectID: drag.projectID)
        store.moveSession(drag.sessionID, toProjectID: insertion.projectID)
        if let node = store.findDisplayNode(insertion.projectID) {
            // The mixed pinned partition (pinned Sessions AND pinned child
            // groups): a pinned insert must keep the groups' ranks in the
            // rewritten pinned order.
            let pinnedIDs = store.renderedPinnedItems(in: node).map(\.id)
            if drag.pinned {
                // A pinned session stays pinned across the move
                // (`moveSession` re-pins it at the destination); position it
                // within the destination's pinned section.
                if let ids = Self.insertionOrder(
                    ids: pinnedIDs,
                    draggedID: drag.sessionID,
                    anchorID: insertion.anchorID,
                    below: insertion.below
                ) {
                    store.setPinnedOrder(projectID: insertion.projectID, ids: ids)
                }
            } else {
                let pinnedSet = Set(pinnedIDs)
                // The MIXED rendered list, exactly like commitSessionReorder:
                // the destination's child groups keep their rank in the
                // written order. Building the order from sessions alone
                // dropped the folder ids, and an unranked group jumps to the
                // top of the destination project's list.
                let regularIDs = store.renderedDisplayedItems(in: node)
                    .map(\.id)
                    .filter { !pinnedSet.contains($0) }
                if let ids = Self.insertionOrder(
                    ids: regularIDs,
                    draggedID: drag.sessionID,
                    anchorID: insertion.anchorID,
                    below: insertion.below
                ) {
                    // A positional drop into a date-sorted list IS choosing
                    // custom order there — without the flip the dropped
                    // position would snap back to date order on rescan.
                    if store.isDateSorted(projectID: insertion.projectID) {
                        store.setSessionDateSorted(false, for: insertion.projectID)
                    }
                    store.setSessionOrder(projectID: insertion.projectID, ids: ids)
                }
            }
        }
        dragState.end()
        fadeOutCard(towardRow: insertion.anchorID)
    }

    /// The final order for a positional cross-group insert: the target
    /// section's current ids with the dragged id placed into the gap
    /// above/below the anchor row. Nil when the anchor vanished (the caller
    /// keeps the plain `moveSession` filing).
    nonisolated static func insertionOrder(
        ids: [String], draggedID: String, anchorID: String, below: Bool
    ) -> [String]? {
        var ids = ids
        ids.removeAll { $0 == draggedID }
        guard let anchor = ids.firstIndex(of: anchorID) else { return nil }
        ids.insert(draggedID, at: anchor + (below ? 1 : 0))
        return ids
    }

    /// The pure transform-gap rule: which way a same-section row shifts
    /// while the gap occupies `targetIndex`. Rows strictly between the
    /// dragged slot and the target move one slot TOWARD the origin (opening
    /// the gap at the target); everything else — including the dragged
    /// row's own invisible slot — stays put. Mirrors exactly what
    /// `previewSessionMove(over:)` produces in real layout at drop.
    /// Zones on a same-section GROUP row under a dragged session: the
    /// middle files into the group, while the top/bottom bands are
    /// positional (the gap opens at the group's slot — the only way to
    /// land directly above/below it). Bands are 30% of the row, at least
    /// 6pt each.
    nonisolated static func folderHoverIsPositional(
        y: CGFloat, rowMinY: CGFloat, rowHeight: CGFloat
    ) -> Bool {
        let band = max(6, rowHeight * 0.3)
        return y < rowMinY + band || y > rowMinY + rowHeight - band
    }

    nonisolated static func reorderShiftDirection(
        rowIndex: Int, draggedIndex: Int, targetIndex: Int
    ) -> Int {
        if targetIndex > draggedIndex,
           rowIndex > draggedIndex, rowIndex <= targetIndex {
            return -1
        }
        if targetIndex < draggedIndex,
           rowIndex < draggedIndex, rowIndex >= targetIndex {
            return 1
        }
        return 0
    }

    /// Shared post-drop card exit: fade at (or drift slightly toward) the
    /// row the drop landed on.
    private func fadeOutCard(towardRow rowID: String) {
        phase = .settling
        stopDragTimer()
        clearHoveredTarget()
        clearInsertion()
        generation += 1
        let gen = generation
        guard !SidebarMotion.reduceMotion, let window = cardWindow else {
            forceClearCard()
            return
        }
        if let state = cardState {
            withAnimation(.easeOut(duration: 0.16)) { state.lifted = false }
        }
        NSAnimationContext.runAnimationGroup { context in
            context.duration = 0.16
            context.timingFunction = CAMediaTimingFunction(name: .easeOut)
            window.animator().alphaValue = 0
            if let dest = cardWindowOrigin(overRow: rowID) {
                window.animator().setFrame(
                    NSRect(origin: dest, size: window.frame.size), display: true
                )
            }
        }
        DispatchQueue.main.asyncAfter(deadline: .now() + 0.18) { [weak self] in
            guard let self, self.generation == gen else { return }
            self.forceClearCard()
        }
    }

    private func forceClearCard() {
        stopDragTimer()
        closeCardWindow()
        dragState?.setLiftedSessionRow(nil)
        dragState?.setLiftedProjectRow(nil)
        projectDropTargetID = nil
        projectDropBelow = false
        phase = .idle
        hoveredTargetProjectID = nil
        hoveredInsertion = nil
        reorderAnchorID = nil
        paneDragContext = nil
        panelDragSessionID = nil
        panelReorderTargetID = nil
        panelInsertTarget = nil
        store?.setPaneSwapPreview(nil)
        store?.setProjectSidebarReorderPreview(nil)
        store?.setProjectSidebarInsertPreview(nil)
        pinnedDragBoundary = nil
        pinnedDropDenied = false
        worktreeDropDenied = false
        selectionBeforePressedSession = nil
        clearDropZone()
        cancelSpringOpen()
        // Lift the glide suppression one tick AFTER the land transaction's
        // render committed (see `listGlideSuppressed`). A drag that began
        // in the meantime keeps its own suppression.
        DispatchQueue.main.async { [weak self] in
            guard let self, case .idle = self.phase else { return }
            self.dragState?.setListGlideSuppressed(false)
        }
    }

    private func clearHoveredTarget() {
        if let target = hoveredTargetProjectID {
            dragState?.setSessionDropTarget(target, hovering: false)
            hoveredTargetProjectID = nil
        }
    }

    private func clearInsertion() {
        if hoveredInsertion != nil {
            hoveredInsertion = nil
            dragState?.setSessionInsertion(nil)
        }
    }

    // MARK: Spring-loading (auto-expand collapsed rows)

    /// Hovering a COLLAPSED project/group/folder row for `springOpenDelay`
    /// expands it in place so its contents can take the drop. Leaving the
    /// row first cancels the pending open; an opened row is never
    /// auto-collapsed afterwards.
    private func updateSpringOpen(candidate: String?) {
        guard candidate != springOpenCandidateID else { return }
        springOpenWork?.cancel()
        springOpenWork = nil
        springOpenCandidateID = candidate
        guard let id = candidate, let store,
              !store.expandedProjectIDs.contains(id),
              let node = store.findDisplayNode(id),
              !node.sessions.isEmpty || !node.worktrees.isEmpty
        else { return }
        let work = DispatchWorkItem { [weak self] in
            MainActor.assumeIsolated { self?.fireSpringOpen(id) }
        }
        springOpenWork = work
        DispatchQueue.main.asyncAfter(
            deadline: .now() + Self.springOpenDelay, execute: work
        )
    }

    private func fireSpringOpen(_ projectID: String) {
        springOpenWork = nil
        guard case .dragging = phase,
              springOpenCandidateID == projectID,
              let store,
              !store.expandedProjectIDs.contains(projectID)
        else { return }
        if SidebarMotion.reduceMotion {
            store.expandedProjectIDs.insert(projectID)
        } else {
            withAnimation(SidebarMotion.accordionOpen) {
                store.expandedProjectIDs.insert(projectID)
            }
        }
        // The newly revealed rows report their frames via onGeometryChange;
        // the next mouse move (or auto-scroll tick) re-hit-tests against
        // them. Re-run once now so a stationary cursor updates too.
        if let view = monitorView {
            let content = view.convert(lastWindowLocation, from: nil)
            hitTest(content)
        }
    }

    private func cancelSpringOpen() {
        springOpenWork?.cancel()
        springOpenWork = nil
        springOpenCandidateID = nil
    }

    // MARK: Auto-scroll

    private func updateAutoScroll(content: CGPoint, in view: NSView) {
        guard let scrollView = view.enclosingScrollView,
              content.x >= 0, content.x <= view.bounds.width
        else { return stopAutoScroll() }
        let visible = scrollView.documentVisibleRect
        let topDistance = content.y - visible.minY
        let bottomDistance = visible.maxY - content.y
        var step: CGFloat = 0
        if topDistance < Self.autoScrollZone {
            step = -Self.autoScrollMaxStep
                * (1 - max(0, topDistance) / Self.autoScrollZone)
        } else if bottomDistance < Self.autoScrollZone {
            step = Self.autoScrollMaxStep
                * (1 - max(0, bottomDistance) / Self.autoScrollZone)
        }
        autoScrollStep = step
        guard step != 0 else { return stopAutoScroll() }
        guard autoScrollTimer == nil else { return }
        let timer = Timer(timeInterval: 1.0 / 60.0, repeats: true) { [weak self] _ in
            // Timer fires on the main runloop; hop is a formality.
            MainActor.assumeIsolated { self?.autoScrollTick() }
        }
        // .common so the timer fires during the mouse-drag tracking loop.
        RunLoop.main.add(timer, forMode: .common)
        autoScrollTimer = timer
    }

    private func autoScrollTick() {
        guard case .dragging = phase,
              autoScrollStep != 0,
              let view = monitorView,
              let scrollView = view.enclosingScrollView
        else { return stopAutoScroll() }
        let clip = scrollView.contentView
        let documentHeight = scrollView.documentView?.frame.height ?? 0
        let maxOriginY = max(0, documentHeight - clip.bounds.height)
        var origin = clip.bounds.origin
        origin.y = min(max(0, origin.y + autoScrollStep), maxOriginY)
        guard origin != clip.bounds.origin else { return stopAutoScroll() }
        clip.setBoundsOrigin(origin)
        scrollView.reflectScrolledClipView(clip)
        // The cursor is stationary but the content moved under it: refresh
        // the preview/target from the new content-space position. The card
        // (its own window) stays put.
        let content = view.convert(lastWindowLocation, from: nil)
        hitTest(content)
    }

    private func stopAutoScroll() {
        autoScrollTimer?.invalidate()
        autoScrollTimer = nil
        autoScrollStep = 0
    }

    // MARK: Card window

    /// Builds the borderless, click-through window that carries the floating
    /// card and orders it above the app window (explicit ordering, NOT
    /// `addChildWindow`: the card is short-lived and self-moved, and the
    /// parent/child frame coupling has nothing to offer a window that must
    /// follow the cursor, not the parent). AppKit `NSWindow` moves are not
    /// implicitly animated, so per-move `setFrameOrigin` calls land
    /// synchronously — the card tracks the cursor exactly.
    private func presentCardWindow(
        card: Card, over parent: NSWindow, rowRectInWindow: NSRect
    ) {
        closeCardWindow()
        let margin = Self.cardWindowMargin
        let state = SidebarSessionDragCardState()
        let contentSize = NSSize(
            width: card.size.width + margin * 2,
            height: card.size.height + margin * 2
        )
        let window = NSWindow(
            contentRect: NSRect(origin: .zero, size: contentSize),
            styleMask: [.borderless],
            backing: .buffered,
            defer: false
        )
        window.isOpaque = false
        window.backgroundColor = .clear
        window.hasShadow = false
        window.ignoresMouseEvents = true
        window.isReleasedWhenClosed = false
        window.animationBehavior = .none
        window.appearance = parent.appearance ?? NSApp.effectiveAppearance
        let host = NSHostingView(
            rootView: SidebarSessionDragCardView(card: card, state: state, margin: margin)
        )
        host.frame = NSRect(origin: .zero, size: contentSize)
        window.contentView = host
        let rowScreen = parent.convertToScreen(rowRectInWindow)
        window.setFrameOrigin(NSPoint(
            x: rowScreen.minX - margin, y: rowScreen.minY - margin
        ))
        window.level = parent.level
        window.order(.above, relativeTo: parent.windowNumber)
        cardWindow = window
        cardState = state
        if SidebarMotion.reduceMotion {
            state.lifted = true
        } else {
            withAnimation(Self.liftAnimation) { state.lifted = true }
        }
    }

    private func moveCardWindow(in view: NSView) {
        guard let window = cardWindow, let parent = view.window, let card else { return }
        // The magnet shifts are continuous functions of the cursor position
        // (0 at the zone center and at the attraction radius), so the
        // per-event window moves stay smooth — no snap on zone entry/exit.
        // Zone-band magnet and sort-target magnet are never active at once
        // (a hovered sort target suppresses the zone bands).
        let sortShift = sortTargetMagnetShift()
        let originInWindow = NSPoint(
            x: lastWindowLocation.x - grabOffset.x
                + dropZoneMagnetShift(in: view) + sortShift.x,
            y: lastWindowLocation.y - grabOffset.y + sortShift.y
        )
        let screen = parent.convertToScreen(
            NSRect(origin: originInWindow, size: card.size)
        )
        window.setFrameOrigin(NSPoint(
            x: screen.minX - Self.cardWindowMargin,
            y: screen.minY - Self.cardWindowMargin
        ))
    }

    /// Screen-space card-window origin that puts the card over a registered
    /// row's current frame (the settle/fade destinations).
    private func cardWindowOrigin(overRow rowID: String) -> NSPoint? {
        guard let record = rows[rowID],
              let view = monitorView,
              let parent = view.window,
              card != nil
        else { return nil }
        let rowRect = view.convert(record.frame, to: nil)
        let screen = parent.convertToScreen(rowRect)
        return NSPoint(
            x: screen.minX - Self.cardWindowMargin,
            y: screen.minY - Self.cardWindowMargin
        )
    }

    private func closeCardWindow() {
        if let window = cardWindow {
            window.orderOut(nil)
        }
        cardWindow = nil
        cardState = nil
        card = nil
    }

    // MARK: Geometry

    private func row(at content: CGPoint) -> (String, RowRecord)? {
        var best: (id: String, record: RowRecord)?
        for (id, record) in rows where record.frame.contains(content) {
            guard let current = best else {
                best = (id, record)
                continue
            }
            let priority = hitTestPriority(record.kind)
            let currentPriority = hitTestPriority(current.record.kind)
            let area = record.frame.width * record.frame.height
            let currentArea = current.record.frame.width * current.record.frame.height
            // A group shell deliberately overlaps every row it contains.
            // Semantic rows win first; among nested semantic/shell frames,
            // the smallest visible region is the most specific target.
            if priority < currentPriority
                || (priority == currentPriority && area < currentArea) {
                best = (id, record)
            }
        }
        return best.map { ($0.id, $0.record) }
    }

    private func hitTestPriority(_ kind: RowKind) -> Int {
        switch kind {
        case .projectShell, .groupShell: return 1
        case .session, .folderItem, .project: return 0
        }
    }

    private func sessionRow(at content: CGPoint) -> (String, RowRecord)? {
        guard let hit = row(at: content), case .session = hit.1.kind else { return nil }
        return hit
    }

    private func draggableRow(at content: CGPoint) -> (String, RowRecord)? {
        guard let hit = row(at: content) else { return nil }
        switch hit.1.kind {
        case .session: return hit.1.isDraggable ? hit : nil
        case .folderItem, .project: return hit
        case .projectShell, .groupShell: return nil
        }
    }

    /// Top-level project containing this point. Header records win over the
    /// larger overlapping block, though both resolve to the same project id.
    /// This scan is intentionally independent of `row(at:)`: nested Session
    /// rows have higher semantic hit priority, but they must not punch holes
    /// in the containing PROJECT's reorder target geometry.
    private func projectBlock(at content: CGPoint) -> (id: String, below: Bool)? {
        var best: (id: String, priority: Int, area: CGFloat, frame: CGRect)?
        for (id, record) in rows where record.frame.contains(content) {
            let projectID: String
            let priority: Int
            switch record.kind {
            case .project:
                projectID = id
                priority = 1
            case let .projectShell(id):
                projectID = id
                // Prefer the full block over its overlapping header: the
                // block midpoint gives a natural above/below insertion slot.
                priority = 0
            case .session, .folderItem, .groupShell:
                continue
            }
            let area = record.frame.width * record.frame.height
            if best == nil
                || priority < best!.priority
                || (priority == best!.priority && area < best!.area) {
                best = (projectID, priority, area, record.frame)
            }
        }
        return best.map { ($0.id, content.y > $0.frame.midY) }
    }

    /// A tiny gap between project blocks should not erase an otherwise valid
    /// target. Returning over the source project does clear it, so releasing
    /// back where the drag started remains a no-op.
    nonisolated static func retainedProjectDropTarget(
        current: String?, hit: String?, draggedID: String
    ) -> String? {
        guard let hit else { return current }
        return hit == draggedID ? nil : hit
    }

    private func folderName(parentID: String, folderID: String) -> String? {
        guard let store else { return nil }
        func search(_ nodes: [ProjectNode]) -> ProjectNode? {
            for node in nodes {
                if node.id == parentID { return node }
                if let found = search(node.worktrees) { return found }
            }
            return nil
        }
        return search(store.displayNodes)?.worktrees.first { $0.id == folderID }?.project.name
    }

    private func sessionEntry(projectID: String, sessionID: String) -> SessionEntry? {
        guard let store else { return nil }
        func search(_ nodes: [ProjectNode]) -> ProjectNode? {
            for node in nodes {
                if node.id == projectID { return node }
                if let found = search(node.worktrees) { return found }
            }
            return nil
        }
        return search(store.displayNodes)?.sessions.first { $0.id == sessionID }
    }

    private func sidebarLocation(
        for sessionID: String
    ) -> (projectID: String, pinned: Bool, depth: Int)? {
        guard let store else { return nil }
        func search(
            _ nodes: [ProjectNode], depth: Int
        ) -> (projectID: String, pinned: Bool, depth: Int)? {
            for node in nodes {
                if node.sessions.contains(where: { $0.id == sessionID }) {
                    return (
                        node.id,
                        store.isPinned(sessionID: sessionID, projectID: node.id),
                        depth
                    )
                }
                if let found = search(node.worktrees, depth: depth + 1) {
                    return found
                }
            }
            return nil
        }
        return search(store.displayNodes, depth: 0)
    }
}

// MARK: - Environment plumbing (no subscription)

private struct SidebarSessionDragControllerKey: EnvironmentKey {
    static let defaultValue: SidebarSessionDragController? = nil
}

extension EnvironmentValues {
    /// Plain environment VALUE (not `@EnvironmentObject`): rows report their
    /// frames through it without subscribing to the controller.
    var sidebarSessionDragController: SidebarSessionDragController? {
        get { self[SidebarSessionDragControllerKey.self] }
        set { self[SidebarSessionDragControllerKey.self] = newValue }
    }
}

/// Registers a sidebar row's content-space frame with the drag controller.
/// `onGeometryChange` keeps the registry fresh across scrolls and the live
/// reorder preview's row moves; the action writes into a plain dictionary —
/// no view invalidation.
struct SidebarSessionDragFrameReporter: ViewModifier {
    @Environment(\.sidebarSessionDragController) private var controller
    let id: String
    let kind: SidebarSessionDragController.RowKind
    let isDraggable: Bool
    /// Per-view-instance registration identity — see removeRowFrame.
    @State private var token = UUID()

    /// `onGeometryChange` normally publishes only when the CGRect changes.
    /// A cross-group move can reparent a row into the exact same coordinates,
    /// though; include its semantic kind so the registry adopts the new owner
    /// even when collapse/expand is the first later geometry change.
    private struct ReportedGeometry: Equatable {
        let frame: CGRect
        let kind: SidebarSessionDragController.RowKind
        let isDraggable: Bool
    }

    func body(content: Content) -> some View {
        content
            .onGeometryChange(for: ReportedGeometry.self) { proxy in
                ReportedGeometry(
                    frame: proxy.frame(in: .named(SidebarSessionDragController.contentSpace)),
                    kind: kind,
                    isDraggable: isDraggable
                )
            } action: { reported in
                controller?.setRowFrame(
                    id: id,
                    kind: reported.kind,
                    frame: reported.frame,
                    isDraggable: reported.isDraggable,
                    token: token
                )
            }
            .onDisappear { controller?.removeRowFrame(id: id, token: token) }
    }
}

extension View {
    func sidebarDragRowFrame(
        id: String,
        kind: SidebarSessionDragController.RowKind,
        isDraggable: Bool = true
    ) -> some View {
        modifier(SidebarSessionDragFrameReporter(
            id: id,
            kind: kind,
            isDraggable: isDraggable
        ))
    }
}

// MARK: - Scoped drag-state observers
//
// SidebarView and ProjectNodeView hold `SidebarDragState` as a plain,
// UNOBSERVED reference (the heavy tree must never re-evaluate per drag-state
// publish — with 100+ sessions that re-built every row's view value on every
// hovered-row change). Everything that renders FROM drag state lives in the
// small modifiers below instead: each carries its own subscription, and a
// publish re-runs only these leaf bodies — `content` stays an opaque
// reference to the already-built row, so the row body is never re-diffed.

/// Disables list hit-testing while a detached session drag is in flight so
/// row hover chrome can't churn under the floating card. Lives in its own
/// modifier for a second reason: the monitor `NSViewRepresentable` attaches
/// OUTSIDE this modifier, so the toggle can never remount it — applying
/// `.allowsHitTesting(dragState.sessionDrag == nil)` above the monitor's
/// `.background` in SidebarView remounted the monitor view (tearing down and
/// re-installing its NSEvent monitor) on every drag begin/end.
struct SidebarSessionDragHitTestGate: ViewModifier {
    @ObservedObject var dragState: SidebarDragState

    func body(content: Content) -> some View {
        content.allowsHitTesting(!dragState.isActive)
    }
}

/// Project/group row drag chrome: dims the source row of an in-flight
/// project drag, and paints the muted "files into this group" fill while the
/// detached session drag hovers this row as an accepted drop target (same
/// hover fill the row itself uses — no accent ring, see the old inline
/// comment: accent read as "new item", not filing).
struct SidebarProjectRowDragEffects: ViewModifier {
    @ObservedObject var dragState: SidebarDragState
    let nodeID: String

    func body(content: Content) -> some View {
        let isDropTarget = dragState.sessionDropTargetProjectID == nodeID
        let isLifted = dragState.liftedProjectRowID == nodeID
        content
            // Match Session dragging: the detached card owns the visible
            // header, while its original row becomes a true empty slot.
            // Expanded descendants stay mounted so this remains the cheap,
            // stable project path that avoids rebuilding large subtrees on
            // every pointer move.
            .opacity(isLifted ? 0 : 1)
            .background(
                RoundedRectangle(cornerRadius: 9, style: .continuous)
                    .fill(isDropTarget ? Theme.hoverRow : .clear)
            )
            .animation(
                SidebarMotion.reduceMotion || !isLifted
                    ? nil
                    : SidebarSessionDragController.slotAnimation,
                value: isLifted
            )
            .animation(.easeInOut(duration: 0.12), value: isDropTarget)
    }
}

/// Top-level project insertion feedback without a tree reorder. Only this
/// small leaf observes the drag state; adding one header-sized padding slot
/// shifts layout cheaply while the Store's project tree remains untouched.
struct SidebarProjectBlockDragEffects: ViewModifier {
    @ObservedObject var dragState: SidebarDragState
    let nodeID: String

    static let insertionGap: CGFloat = 30

    func body(content: Content) -> some View {
        let insertion = dragState.projectInsertion
        let gapAbove = insertion?.projectID == nodeID && insertion?.below == false
        let gapBelow = insertion?.projectID == nodeID && insertion?.below == true
        content
            .padding(.top, gapAbove ? Self.insertionGap : 0)
            .padding(.bottom, gapBelow ? Self.insertionGap : 0)
            .animation(
                SidebarMotion.reduceMotion
                    ? nil
                    : SidebarSessionDragController.slotAnimation,
                value: gapAbove
            )
            .animation(
                SidebarMotion.reduceMotion
                    ? nil
                    : SidebarSessionDragController.slotAnimation,
                value: gapBelow
            )
    }
}

/// Damped horizontal "no" wiggle for a refused pinned-boundary drop. Each
/// unit of `progress` is one shake: the transform is identity at every
/// integer (rest), and inside a step it oscillates three times with the
/// amplitude decaying to zero — so the animated bump from N to N+1 shakes
/// and settles, and a resting row is untransformed.
struct SidebarDeniedShakeEffect: GeometryEffect {
    var progress: CGFloat

    var animatableData: CGFloat {
        get { progress }
        set { progress = newValue }
    }

    nonisolated static let amplitude: CGFloat = 4
    nonisolated static let oscillations: CGFloat = 3

    func effectValue(size _: CGSize) -> ProjectionTransform {
        ProjectionTransform(CGAffineTransform(
            translationX: Self.translationX(progress: progress), y: 0
        ))
    }

    nonisolated static func translationX(progress: CGFloat) -> CGFloat {
        let phase = progress - progress.rounded(.down)
        guard phase > 0 else { return 0 }
        return sin(phase * .pi * 2 * oscillations) * amplitude * (1 - phase)
    }
}

/// Session row drag chrome: the lifted source slot (stays in the list at
/// full height — it IS the live insertion gap the other rows part around —
/// but dims and shrinks slightly with a spring while its content rides the
/// floating card; cleared only after the card lands, so commit/cancel never
/// double-expose the row), plus the cross-group positional insertion bars
/// (set by the drag controller only for lists whose group passes
/// `canMoveSession` — non-accepting lists never preview).
extension SidebarDragState {
    /// A row's (or folder cluster's) transform-gap offset. Rows outside the
    /// frozen origin-section map trivially return 0; the hot cost per
    /// published target change is one dictionary lookup per visible row.
    func reorderShift(for id: String, inProject projectID: String) -> CGFloat {
        guard let target = sessionReorderTargetIndex,
              sessionDrag?.projectID == projectID,
              let mine = sessionReorderIndexByID[id]
        else { return 0 }
        return CGFloat(SidebarSessionDragController.reorderShiftDirection(
            rowIndex: mine,
            draggedIndex: sessionReorderDraggedIndex,
            targetIndex: target
        )) * sessionReorderShiftDistance
    }
}

struct SidebarSessionRowDragEffects: ViewModifier {
    @ObservedObject var dragState: SidebarDragState
    let sessionID: String
    /// The row's owning group — insertion bars match on (project, anchor).
    let projectID: String
    /// False for an inline folder HEADER: its whole cluster shifts as one
    /// unit via `SidebarFolderClusterDragEffects` instead, so the header
    /// must not also move on its own.
    var shiftsWithReorder = true

    /// Height of the slot that opens for a cross-group positional insert —
    /// a row-sized gap, so the drop target reads as "the card lands here".
    static let insertionGap: CGFloat = 26

    func body(content: Content) -> some View {
        let isLifted = dragState.liftedSessionRowID == sessionID
        // Card outside the sidebar: the empty origin slot closes entirely
        // (the row "left the list"); it reopens when the card comes back.
        let isCollapsed = isLifted && dragState.liftedSlotCollapsed
        let gapAbove = matchesInsertion(below: false)
        let gapBelow = matchesInsertion(below: true)
        let shift = shiftsWithReorder
            ? dragState.reorderShift(for: sessionID, inProject: projectID)
            : 0
        // The lifted row renders as pure empty space at its ORIGINAL layout
        // slot; the same-section reorder gap is a pure `offset` — rows
        // between the origin slot and the hovered target ride one slot
        // toward the origin, opening the gap at the target without any
        // layout or tree work (see `sessionReorderTargetIndex`). Cross-group
        // inserts open a row-sized layout gap against the anchor instead.
        // The registered drag frame wraps this modifier, so row frames stay
        // static through the whole drag — hit-testing is against geometry
        // that never moves under the cursor.
        content
            .opacity(isLifted ? 0 : 1)
            .frame(height: isCollapsed ? 0 : nil)
            .clipped()
            .offset(y: shift)
            // Refused-drop "no" shake (pinned Sessions/groups). The beat only
            // ever grows, and only for this row, so the effect rests at
            // identity for everyone else.
            .modifier(SidebarDeniedShakeEffect(
                progress: CGFloat(dragState.deniedShakeBeats[sessionID] ?? 0)
            ))
            .padding(.top, gapAbove ? Self.insertionGap : 0)
            .padding(.bottom, gapBelow ? Self.insertionGap : 0)
            // While the drag is active the shift springs (rows part and
            // close live); the drop's final clear happens in the same
            // transaction that ends the drag, so the new value resolves a
            // nil animation — the transform hands off to the identical real
            // layout with no double motion.
            .animation(
                SidebarMotion.reduceMotion || dragState.sessionDrag == nil
                    ? nil
                    : SidebarSessionDragController.slotAnimation,
                value: shift
            )
            .animation(
                SidebarMotion.reduceMotion
                    ? nil
                    : SidebarSessionDragController.slotAnimation,
                value: isCollapsed
            )
            // Hide animates (the row dissolves under the lifting card);
            // reveal is INSTANT — it fires in the same beat the landed card
            // window closes, and the card content is sitting exactly over
            // the slot, so an animated fade-in here reads as the dropped
            // row blinking out and back.
            .animation(
                SidebarMotion.reduceMotion || !isLifted
                    ? nil
                    : SidebarSessionDragController.slotAnimation,
                value: isLifted
            )
            .animation(
                SidebarMotion.reduceMotion
                    ? nil
                    : SidebarSessionDragController.slotAnimation,
                value: gapAbove
            )
            .animation(
                SidebarMotion.reduceMotion
                    ? nil
                    : SidebarSessionDragController.slotAnimation,
                value: gapBelow
            )
    }

    private func matchesInsertion(below: Bool) -> Bool {
        guard let insertion = dragState.sessionInsertion else { return false }
        return insertion.projectID == projectID
            && insertion.anchorID == sessionID
            && insertion.below == below
    }
}

/// Whole-cluster transform shift for an inline folder: the header, its
/// expanded contents, and the group wash ride the same-section gap as ONE
/// unit (an in-between folder shifts by the same one-slot stride as any
/// row). Offsetting just the header row tore it out of its cluster —
/// empty space opened inside and below the group.
struct SidebarFolderClusterDragEffects: ViewModifier {
    @ObservedObject var dragState: SidebarDragState
    let folderID: String
    let parentID: String

    func body(content: Content) -> some View {
        let shift = dragState.reorderShift(for: folderID, inProject: parentID)
        content
            .offset(y: shift)
            .animation(
                SidebarMotion.reduceMotion || dragState.sessionDrag == nil
                    ? nil
                    : SidebarSessionDragController.slotAnimation,
                value: shift
            )
    }
}

// MARK: - Event monitor view

/// Invisible NSView spanning the sidebar list content. It never hit-tests;
/// it exists to (a) install the local mouse/key monitor, (b) convert event
/// locations into the shared content coordinate space (isFlipped matches
/// SwiftUI's top-left origin), and (c) reach the enclosing NSScrollView for
/// edge auto-scroll.
struct SidebarSessionDragMonitor: NSViewRepresentable {
    let controller: SidebarSessionDragController
    let store: UnpeelStore
    let dragState: SidebarDragState

    func makeNSView(context _: Context) -> MonitorView {
        let view = MonitorView()
        view.controller = controller
        controller.bind(store: store, dragState: dragState, monitorView: view)
        return view
    }

    func updateNSView(_ view: MonitorView, context _: Context) {
        view.controller = controller
        controller.bind(store: store, dragState: dragState, monitorView: view)
    }

    final class MonitorView: NSView {
        weak var controller: SidebarSessionDragController?
        private var monitor: Any?

        override var isFlipped: Bool { true }
        override func hitTest(_: NSPoint) -> NSView? { nil }

        override func viewDidMoveToWindow() {
            super.viewDidMoveToWindow()
            removeMonitorIfNeeded()
            guard window != nil else {
                controller?.monitorDetached()
                return
            }
            monitor = NSEvent.addLocalMonitorForEvents(
                matching: [.leftMouseDown, .leftMouseDragged, .leftMouseUp, .keyDown]
            ) { [weak self] event in
                guard let self, let controller = self.controller,
                      event.window === self.window
                else { return event }
                return controller.handle(event, in: self)
            }
        }

        private func removeMonitorIfNeeded() {
            if let monitor {
                NSEvent.removeMonitor(monitor)
                self.monitor = nil
            }
        }
    }
}

// MARK: - Floating card content

/// Rare-change card chrome state (lift scale/shadow) inside the card window.
/// Changes only at lift and settle — never per mouse move.
@MainActor
final class SidebarSessionDragCardState: ObservableObject {
    @Published var lifted = false
}

/// The detached row card content hosted inside the card window. All row
/// state is the lift-time snapshot on `Card`; nothing here observes the
/// store, so the card costs nothing while it rides the cursor.
private struct SidebarSessionDragCardView: View {
    let card: SidebarSessionDragController.Card
    @ObservedObject var state: SidebarSessionDragCardState
    let margin: CGFloat

    init(
        card: SidebarSessionDragController.Card,
        state: SidebarSessionDragCardState,
        margin: CGFloat
    ) {
        self.card = card
        self.state = state
        self.margin = margin
    }

    var body: some View {
        let shape = RoundedRectangle(cornerRadius: 9, style: .continuous)
        Group {
            if let session = card.session {
                SessionRowView(
                    session: session,
                    depth: card.depth,
                    indentBase: 9,
                    isSelected: card.isSelected,
                    isUnread: card.isUnread,
                    onSelect: {}
                ).equatable()
            } else if let projectName = card.projectName {
                HStack(spacing: 7) {
                    ChromeIconView(
                        icon: card.projectExpanded ? .folderOpen : .folderClosed,
                        size: 16
                    )
                    .foregroundStyle(
                        card.projectFolderColor?.tint ?? Theme.mutedForeground
                    )
                    .frame(width: 18, height: 18)
                    Text(projectName)
                        .font(Theme.rowLabelFont)
                        .foregroundStyle(Theme.foreground.opacity(0.8))
                        .lineLimit(1)
                    Spacer(minLength: 4)
                }
                .padding(EdgeInsets(top: 2, leading: 7, bottom: 2, trailing: 9))
            } else {
                HStack(spacing: 7) {
                    ChevronGlyph(glyph: "›", size: 17)
                        .foregroundStyle(Theme.mutedForeground)
                        .opacity(0.6)
                        .frame(width: 11, height: 18)
                    Text(card.folderName ?? "")
                        .font(Theme.sessionLabelFont)
                        .foregroundStyle(Theme.foreground)
                        .lineLimit(1)
                    Spacer(minLength: 4)
                }
                .padding(EdgeInsets(
                    top: 2,
                    leading: 9 + CGFloat(max(0, card.depth - 1)) * 14,
                    bottom: 2,
                    trailing: 9
                ))
            }
        }
        .frame(width: card.size.width, height: card.size.height)
        .background {
            // The active session's card carries the active-row wash (the
            // row inside already renders its selected chrome); everything
            // else keeps the lighter hover wash. The whole backdrop keys on
            // `lifted`: it fades IN with the lift and dissolves back to
            // transparent while the card settles into a slot, so by landing
            // the card is pixel-identical to the plain row it swaps with.
            shape.fill(.ultraThinMaterial)
                .overlay(shape.fill(
                    card.isSelected
                        ? AnyShapeStyle(Theme.activeRow)
                        : AnyShapeStyle(Theme.hoverRow)
                ))
                .overlay(shape.stroke(Theme.foreground.opacity(0.08), lineWidth: 1))
                .opacity(state.lifted ? 1 : 0)
        }
        .compositingGroup()
        .scaleEffect(state.lifted ? SidebarSessionDragController.liftScale : 1)
        .shadow(
            color: .black.opacity(state.lifted ? 0.30 : 0),
            radius: state.lifted ? 16 : 3,
            x: 0,
            y: state.lifted ? 8 : 1
        )
        .padding(margin)
        .allowsHitTesting(false)
    }
}
