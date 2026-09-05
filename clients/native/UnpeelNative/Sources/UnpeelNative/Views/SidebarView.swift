//
//  SidebarView.swift
//  UnpeelNative
//
//  The project/session tree (DESIGN.md §4): project rows, session rows,
//  pinned sessions, worktree child projects, the worktrees slide-in view,
//  and the footer strip.
//
//  Motion values are extracted from the Svelte app:
//  - Worktrees slide-in: fly x ±140, 200ms cubicOut (Sidebar.svelte:465,491)
//  - Accordion open: height 340ms cubic-bezier(0.16,1,0.3,1); content fade
//    220ms ease-out from translateY(-6) scale(0.992) (ProjectItem.svelte:2225-2262)
//  - Accordion close: 240ms cubicInOut, content fade 140ms (ProjectItem.svelte:568-613)
//  - Row entrance: 380ms cubic-bezier(0.18,0.86,0.26,1), stagger 14ms/row,
//    from translate(-5,-4) scale(0.988) (ProjectItem.svelte:2264-2270, 2468-2482)
//

import AppKit
import SwiftUI
import UniformTypeIdentifiers

// MARK: - Drag-to-reorder state

/// Tracks the in-flight sidebar drag (one at a time): either a project/worktree
/// sibling row or a regular session row. Both use the manual NSEvent-driven
/// detached drag in SidebarSessionDrag.swift. The Svelte app
/// reorders projects with svelte-dnd-action (Sidebar.svelte:492-503 →
/// reorder_projects); within-project session reordering is native-only.
///
/// The detached controller owns mouse-up/Esc commit/cancel deterministically.
/// `arm()` remains available for any future system-drag caller, but both
/// current row kinds begin with `armed: false`.
@MainActor
final class SidebarDragState: ObservableObject {
    struct SessionDrag: Equatable {
        let projectID: String
        let sessionID: String
        /// Whether the drag started from a pinned row. A pinned drag only
        /// reorders over other pinned rows in this group; it may still create
        /// a pane, but a cross-group sidebar filing is refused. A regular drag
        /// only reorders over regular rows.
        let pinned: Bool
    }

    @Published private(set) var projectID: String?
    @Published private(set) var sessionDrag: SessionDrag?
    /// Group or root-project row currently accepting the in-flight session
    /// drag (gated by `UnpeelStore.canMoveSession`). Worktree rows never
    /// become targets: moving a running session there would imply changing
    /// its working directory, which requires an explicit restart.
    @Published private(set) var sessionDropTargetProjectID: String?
    /// Positional gap inside ANOTHER group's session list that the in-flight
    /// session drag is hovering (cross-group insert). The anchor row renders
    /// the insertion bar above/below itself; the drop files the session into
    /// `projectID` AND places it at this gap. Only set for targets that pass
    /// `canMoveSession` — lists whose group can't accept never preview.
    struct SessionInsertion: Equatable {
        let projectID: String
        let anchorID: String
        /// The gap below the anchor row (else above it).
        let below: Bool
    }

    @Published private(set) var sessionInsertion: SessionInsertion?

    /// Lightweight top-level project insertion slot. Unlike Session reorder
    /// previews this never rebuilds the tree while the pointer moves; a small
    /// leaf modifier adds the visible gap around the target block.
    struct ProjectInsertion: Equatable {
        let projectID: String
        let below: Bool
    }

    @Published private(set) var projectInsertion: ProjectInsertion?

    /// Session row currently detached as the floating drag card. Set by the
    /// manual drag controller at lift and cleared only after the card lands,
    /// so the source slot stays dimmed through the settle animation (it
    /// deliberately survives `finish()`/`end()`, which fire before the card
    /// finishes flying home).
    @Published private(set) var liftedSessionRowID: String?

    /// Top-level project row currently represented by the detached card.
    /// Its header becomes an empty slot until an accepted drop snaps into
    /// place (or a cancelled drag finishes springing home); its expanded
    /// subtree remains visible throughout.
    @Published private(set) var liftedProjectRowID: String?

    /// True while the drag card rides OUTSIDE the sidebar (over the
    /// terminal): the origin's empty slot collapses to nothing so the list
    /// reads as "the row left", and reopens when the card comes back.
    @Published private(set) var liftedSlotCollapsed = false

    /// Same-section reorder gap, driven WITHOUT touching the store. The
    /// origin section's order and geometry are frozen at lift; while the
    /// card rides over a sibling row only this index changes, and each row's
    /// `SidebarSessionRowDragEffects` derives a pure visual `offset` from it
    /// (rows between the origin slot and the target shift one slot toward
    /// the origin, opening the gap at the target). The tree itself reorders
    /// exactly once, at drop. The previous design ran `previewSessionMove`
    /// per crossed row, which rebuilt the whole project tree and
    /// re-evaluated every sidebar row per mouse move — the drag visibly
    /// stuttered on large sidebars.
    @Published private(set) var sessionReorderTargetIndex: Int?

    /// Frozen origin-section order (row id → index, dragged row included),
    /// captured at lift. Deliberately NOT published — written once per drag,
    /// read by the leaf modifiers when the target index changes.
    private(set) var sessionReorderIndexByID: [String: Int] = [:]
    private(set) var sessionReorderDraggedIndex = 0
    /// Top item of the frozen section — hovering the origin's own header
    /// anchors the gap here, the only way to place a session ABOVE a
    /// group/folder that sits first in the list. Nil when the header is
    /// NOT adjacent to the dragged row's section (a regular drag while
    /// pinned rows sit under the header): a top gap opening below the
    /// untouched pinned block, far from the cursor, read as noise.
    private(set) var sessionReorderFirstID: String?
    /// One slot's layout stride (dragged row height + list spacing): the
    /// exact distance the in-between rows shift.
    private(set) var sessionReorderShiftDistance: CGFloat = 0

    /// True from a session-drag lift until one runloop tick AFTER the drop
    /// lands. Read (unobserved, at render time) by the session list's
    /// id-order glide animation, which must not replay the drop's
    /// already-in-place reorder as a bounce. Deliberately NOT cleared by
    /// `clear()` — everything cleared inside the land transaction reads as
    /// already-idle by the commit render, so only a value that OUTLIVES
    /// that transaction can gate it. The drag controller clears it on the
    /// next tick.
    private(set) var listGlideSuppressed = false

    /// See `listGlideSuppressed`; owned by the detached-drag controller.
    func setListGlideSuppressed(_ suppressed: Bool) {
        listGlideSuppressed = suppressed
    }

    /// Per-row "no" shakes: a pinned Session/group may lift for reordering or
    /// pane placement, but a refused drop across its pinned boundary returns
    /// home and wiggles. Monotonic per-row beats (never reset) so one row's
    /// shake can never replay another's — the leaf modifier animates each
    /// unit step of its own row's count.
    @Published private(set) var deniedShakeBeats: [String: Int] = [:]

    func bumpDeniedShake(_ rowID: String) {
        deniedShakeBeats[rowID, default: 0] += 1
    }

    /// Number of rows currently reporting a Finder folder drag hovering over
    /// them. A counter (not a Bool) so that moving between adjacent rows —
    /// where AppKit may fire the new row's `dropEntered` before the old row's
    /// `dropExited` — never dips to zero and flickers the drop highlight off.
    @Published private(set) var folderHoverCount = 0

    private var monitor: Any?
    private var commitReorder: (() -> Void)?
    private var cancelReorder: (() -> Void)?

    var isActive: Bool { projectID != nil || sessionDrag != nil }

    var isFolderHovered: Bool { folderHoverCount > 0 }

    func folderHoverEnter() { folderHoverCount += 1 }
    func folderHoverExit() { folderHoverCount = max(0, folderHoverCount - 1) }
    func folderHoverReset() { folderHoverCount = 0 }

    func beginProject(
        _ id: String,
        armed: Bool = true,
        commitReorder: @escaping () -> Void,
        cancelReorder: @escaping () -> Void
    ) {
        end()
        projectID = id
        self.commitReorder = commitReorder
        self.cancelReorder = cancelReorder
        if armed { arm() }
    }

    /// `armed: false` is the manual detached-drag path: its controller ends
    /// the drag deterministically on mouse-up/Esc, so the leak-cancel monitor
    /// (which would misfire on the manual drag's own mouse/key events) stays
    /// out of the way.
    func beginSession(
        projectID: String,
        sessionID: String,
        pinned: Bool,
        armed: Bool = true,
        commitReorder: @escaping () -> Void,
        cancelReorder: @escaping () -> Void
    ) {
        end()
        sessionDrag = SessionDrag(
            projectID: projectID, sessionID: sessionID, pinned: pinned
        )
        self.commitReorder = commitReorder
        self.cancelReorder = cancelReorder
        if armed { arm() }
    }

    /// See `liftedSessionRowID`; owned by the detached-drag controller.
    func setLiftedSessionRow(_ sessionID: String?) {
        guard liftedSessionRowID != sessionID else { return }
        liftedSessionRowID = sessionID
        if sessionID == nil { liftedSlotCollapsed = false }
    }

    func setLiftedProjectRow(_ projectID: String?) {
        guard liftedProjectRowID != projectID else { return }
        liftedProjectRowID = projectID
    }

    /// See `liftedSlotCollapsed`; owned by the detached-drag controller.
    func setLiftedSlotCollapsed(_ collapsed: Bool) {
        guard liftedSlotCollapsed != collapsed else { return }
        liftedSlotCollapsed = collapsed
    }

    /// See `sessionReorderTargetIndex`; called once at lift with the origin
    /// section's frozen order. A dragged id missing from the section (stale
    /// registry) leaves the map empty, which disables the transform gap for
    /// this drag — the drop then simply settles home.
    func beginSessionReorderMap(
        ids: [String],
        draggedID: String,
        shiftDistance: CGFloat,
        headerAnchorsTop: Bool
    ) {
        guard let dragged = ids.firstIndex(of: draggedID) else {
            sessionReorderIndexByID = [:]
            sessionReorderFirstID = nil
            return
        }
        var map: [String: Int] = [:]
        for (index, id) in ids.enumerated() { map[id] = index }
        sessionReorderIndexByID = map
        sessionReorderDraggedIndex = dragged
        sessionReorderShiftDistance = shiftDistance
        sessionReorderFirstID = headerAnchorsTop ? ids.first : nil
    }

    /// See `sessionReorderTargetIndex`; owned by the detached-drag
    /// controller, which only calls this when the hovered slot changes.
    func setSessionReorderTarget(_ index: Int?) {
        guard sessionReorderTargetIndex != index else { return }
        sessionReorderTargetIndex = index
    }

    func setSessionDropTarget(_ projectID: String, hovering: Bool) {
        if hovering {
            sessionDropTargetProjectID = projectID
        } else if sessionDropTargetProjectID == projectID {
            sessionDropTargetProjectID = nil
        }
    }

    /// See `sessionInsertion`; owned by the detached-drag controller, which
    /// only calls this when the hovered gap actually changes.
    func setSessionInsertion(_ insertion: SessionInsertion?) {
        guard sessionInsertion != insertion else { return }
        sessionInsertion = insertion
    }

    func setProjectInsertion(_ insertion: ProjectInsertion?) {
        guard projectInsertion != insertion else { return }
        projectInsertion = insertion
    }

    /// Complete a drop accepted by the sidebar: both drag kinds persist
    /// their in-memory preview exactly once here.
    func finish() {
        let commit = commitReorder
        commitReorder = nil
        cancelReorder = nil
        clear()
        commit?()
    }

    /// Cancel a drag that ended outside an accepting drop target. Any
    /// preview is removed so the rows return to their last persisted order.
    func end() {
        let cancel = cancelReorder
        commitReorder = nil
        cancelReorder = nil
        clear()
        cancel?()
    }

    private func clear() {
        projectID = nil
        sessionDrag = nil
        sessionDropTargetProjectID = nil
        sessionInsertion = nil
        projectInsertion = nil
        sessionReorderTargetIndex = nil
        sessionReorderIndexByID = [:]
        sessionReorderDraggedIndex = 0
        sessionReorderShiftDistance = 0
        sessionReorderFirstID = nil
        disarm()
    }

    private func arm() {
        disarm()
        monitor = NSEvent.addLocalMonitorForEvents(
            matching: [.leftMouseDown, .mouseMoved, .keyDown]
        ) { [weak self] event in
            Task { @MainActor in self?.end() }
            return event
        }
    }

    private func disarm() {
        if let monitor {
            NSEvent.removeMonitor(monitor)
            self.monitor = nil
        }
    }
}

/// Row-level drop delegate that previews a live reorder on hover (the moving
/// rows provide the target gap) and commits it when the drop lands.
///
/// It also accepts a Finder **folder** dragged directly onto the row as an
/// "add project" drop. A row must register a UTI to claim its area, and a
/// Finder drag exposes a plain-text path alongside its file URL — so without
/// this the row would swallow the folder drag and reject it (the internal
/// reorder isn't active), and the drop would never fall through to the list's
/// `.onDrop(of: [.fileURL])`. That is why folders only dropped in the gaps
/// between rows before.
private struct SidebarReorderDropDelegate: DropDelegate {
    /// Whether an internal sidebar reorder is in flight (vs. a foreign drag).
    /// Internal project/session reorders use the manual detached drag
    /// (SidebarSessionDrag.swift), which hit-tests row frames itself and
    /// never creates a system drag session. This delegate handles Finder
    /// folder drops that share those row surfaces.
    let isReorderActive: () -> Bool
    /// Returns true when the hovered row accepted the dragged id.
    let moveOver: () -> Bool
    /// Successful reorder drop: persist the drag's preview exactly once,
    /// then clear the drag state.
    let finishReorder: () -> Void
    /// Toggles the list-wide folder-drop highlight while a folder hovers.
    let setFolderHover: (Bool) -> Void
    /// Adds the dragged folders as projects; returns true if any were accepted.
    let addFolders: ([NSItemProvider]) -> Bool

    /// A foreign Finder folder drag (not our own reorder) carrying a file URL.
    private func isFolderDrop(_ info: DropInfo) -> Bool {
        !isReorderActive()
            && info.hasItemsConforming(to: [.fileURL])
    }

    func validateDrop(info: DropInfo) -> Bool {
        isReorderActive() || isFolderDrop(info)
    }

    func dropEntered(info: DropInfo) {
        if isReorderActive() {
            _ = moveOver()
        } else if isFolderDrop(info) {
            setFolderHover(true)
        }
    }

    func dropExited(info: DropInfo) {
        if isFolderDrop(info) {
            setFolderHover(false)
        }
    }

    func dropUpdated(info: DropInfo) -> DropProposal? {
        if isReorderActive() {
            return DropProposal(operation: .move)
        }
        if isFolderDrop(info) { return DropProposal(operation: .copy) }
        return DropProposal(operation: .cancel)
    }

    func performDrop(info: DropInfo) -> Bool {
        if isReorderActive() {
            finishReorder()
            return true
        }
        guard isFolderDrop(info) else { return false }
        setFolderHover(false)
        return addFolders(info.itemProviders(for: [.fileURL]))
    }
}

/// Container-level fallback covering the whole sidebar list area. Without
/// it, the gaps BETWEEN rows (LazyVStack spacing), non-draggable rows and
/// the empty space below the tree have no drop delegate, so AppKit falls
/// back to the default `.copy` proposal — the green "+" badge on the
/// cursor. During an active sidebar drag this proposes `.move` everywhere
/// (the live preview already happened in the row delegates' dropEntered);
/// foreign drags (e.g. files from Finder) are not claimed.
private struct SidebarContainerDropDelegate: DropDelegate {
    /// Whether a sidebar row drag is in flight.
    let isDragActive: () -> Bool
    let finish: () -> Void

    func validateDrop(info _: DropInfo) -> Bool { isDragActive() }

    func dropUpdated(info _: DropInfo) -> DropProposal? {
        DropProposal(operation: isDragActive() ? .move : .cancel)
    }

    func performDrop(info _: DropInfo) -> Bool {
        let active = isDragActive()
        finish()
        return active
    }
}

/// Invisible 1×1 drag preview: hides the floating row-snapshot ghost so the
/// only drag feedback is the live gap animation in the list itself (macOS
/// renders whatever preview view we hand it — a clear pixel reads as none).
private struct EmptyDragPreview: View {
    var body: some View {
        Color.clear.frame(width: 1, height: 1)
    }
}

/// List-wide "drop folder to add project" highlight. A separate view with its
/// OWN dragState subscription (SidebarView holds dragState unobserved): the
/// per-row folder-hover counter churn during a Finder drag re-runs only this
/// overlay body, never the heavy sidebar tree.
struct SidebarFolderDropHighlight: View {
    @ObservedObject var dragState: SidebarDragState
    /// The container-level `.onDrop(isTargeted:)` truth (folder over empty
    /// list space), owned by SidebarView as plain `@State`.
    let externallyTargeted: Bool

    var body: some View {
        let show = externallyTargeted || dragState.isFolderHovered
        ZStack {
            if show {
                RoundedRectangle(cornerRadius: 12, style: .continuous)
                    .fill(Theme.accent.opacity(0.08))
                    .overlay(
                        RoundedRectangle(cornerRadius: 12, style: .continuous)
                            .stroke(Theme.accent.opacity(0.55), lineWidth: 1)
                    )
                    .padding(6)
                    .transition(.opacity)
            }
        }
        .allowsHitTesting(false)
        .animation(.easeInOut(duration: 0.12), value: show)
    }
}

// MARK: - Motion constants (Svelte parity)

enum SidebarMotion {
    /// Svelte `fly` default easing is cubicOut ≈ cubic-bezier(0.33, 1, 0.68, 1).
    static let slide = Animation.timingCurve(0.33, 1, 0.68, 1, duration: 0.2)
    /// Accordion open: 340ms cubic-bezier(0.16, 1, 0.3, 1).
    static let accordionOpen = Animation.timingCurve(0.16, 1, 0.3, 1, duration: 0.34)
    /// Accordion close: 240ms cubicInOut ≈ cubic-bezier(0.65, 0, 0.35, 1).
    static let accordionClose = Animation.timingCurve(0.65, 0, 0.35, 1, duration: 0.24)
    /// Row entrance: 380ms cubic-bezier(0.18, 0.86, 0.26, 1).
    static func rowEnter(index: Int) -> Animation {
        .timingCurve(0.18, 0.86, 0.26, 1, duration: 0.38)
            .delay(Double(index) * 0.014)
    }

    static var reduceMotion: Bool {
        NSWorkspace.shared.accessibilityDisplayShouldReduceMotion
    }
}

/// Session-row entrance: opacity 0 → 1, translate(-5px, -4px) → 0,
/// scale 0.988 → 1 (ProjectItem.svelte @keyframes session-list-item-enter).
private struct SessionRowEnterModifier: ViewModifier {
    let active: Bool

    func body(content: Content) -> some View {
        content
            .opacity(active ? 0 : 1)
            .scaleEffect(active ? 0.988 : 1, anchor: .topLeading)
            .offset(x: active ? -5 : 0, y: active ? -4 : 0)
    }
}

/// Session-list content fade: opacity + translateY(-6) scale(0.992)
/// (ProjectItem.svelte .session-list.native-accordion-list).
private struct SessionListContentModifier: ViewModifier {
    let active: Bool

    func body(content: Content) -> some View {
        content
            .opacity(active ? 0 : 1)
            .scaleEffect(active ? 0.992 : 1, anchor: .top)
            .offset(y: active ? -6 : 0)
    }
}

extension AnyTransition {
    /// Per-row staggered entrance; rows simply fade with the collapsing
    /// shell on removal (140ms, ProjectItem.svelte sessionListContentOutro).
    static func sessionRowStagger(index: Int) -> AnyTransition {
        if SidebarMotion.reduceMotion { return .opacity }
        return .asymmetric(
            insertion: .modifier(
                active: SessionRowEnterModifier(active: true),
                identity: SessionRowEnterModifier(active: false)
            )
            .animation(SidebarMotion.rowEnter(index: index)),
            removal: .opacity.animation(.easeOut(duration: 0.14))
        )
    }

    /// The session-list container: fade in 220ms ease-out from
    /// translateY(-6) scale(0.992); fade out 140ms.
    static var sessionListContent: AnyTransition {
        if SidebarMotion.reduceMotion { return .opacity }
        return .asymmetric(
            insertion: .modifier(
                active: SessionListContentModifier(active: true),
                identity: SessionListContentModifier(active: false)
            )
            .animation(.easeOut(duration: 0.22)),
            removal: .opacity.animation(.easeOut(duration: 0.14))
        )
    }

    /// Sidebar pane slide (Sidebar.svelte:465,491): the project tree flies
    /// to x:-140, the settings nav flies in from x:+140; both fade.
    static func sidebarPanel(fromTrailing: Bool) -> AnyTransition {
        .offset(x: fromTrailing ? 140 : -140).combined(with: .opacity)
    }
}

// MARK: - Sidebar

struct SidebarView: View {
    @ObservedObject var store: UnpeelStore

    /// Shared drag-reorder state for the whole tree (top-level projects,
    /// inline worktree children, sessions). Deliberately plain `@State`
    /// (stable reference, NO subscription — same discipline as the pager
    /// below): its per-drag `@Published` churn (lift, hovered drop target,
    /// insertion gap) re-runs only the tiny observing leaves
    /// (`SidebarSessionDragHitTestGate`, the per-row drag-effect modifiers,
    /// the folder-drop highlight), never this heavy tree body.
    @State private var dragState = SidebarDragState()
    @State private var folderDropTargeted = false
    /// Non-nil presents the shared AddWorkspaceSheet (same sheet Settings ▸
    /// Workspaces opens), launched from the footer "+" popover.
    @State private var addWorkspaceSheetMethod: AddWorkspaceSheet.Method?
    /// Live workspace-carousel state. Deliberately plain `@State` (stable
    /// reference, no subscription): per-scroll-event updates re-run only the
    /// carousel container and the footer dots — never this heavy tree body.
    @State private var workspacePager = SidebarWorkspacePager()
    /// Detached session-row drag ("Dia feel"). Same discipline as the swipe
    /// state: a stable reference held WITHOUT subscription, so per-mouse-move
    /// published updates re-run only the floating-card overlay, never this
    /// heavy tree body. Owned by RootView (plain `@State` there), which also
    /// hosts the floating-card overlay at the WINDOW level so the card can
    /// cross the sidebar edge and render above the Metal terminal.
    let sessionDragController: SidebarSessionDragController

    /// Absolute-top scroll anchor for the project tree (a 1pt marker pinned
    /// to the padded content's top edge, so `anchor: .top` means offset 0).
    private static let treeTopScrollID = "unpeel.sidebar.tree-top"

    var body: some View {
        VStack(spacing: 0) {
            // The list area slides between the project tree and the settings
            // nav; the footer below stays put (Sidebar.svelte .sidebar-views).
            // The animation is scoped to this ZStack on purpose: the settings
            // open/close must never be a window-wide transaction, or the
            // content pane's Metal-backed terminal would get pulled into a
            // transition. (Worktrees render inline in the tree — there is no
            // worktrees pane anymore.)
            // Remote scope renders through the SAME tree: the store projects
            // the selected Host's bootstrap into the display nodes, so there
            // is no separate remote sidebar hierarchy.
            ZStack {
                if store.settingsVisible {
                    SettingsSidebarPanel(store: store)
                        .transition(.sidebarPanel(fromTrailing: true))
                } else {
                    // The workspace carousel: ONE fixed-frame region exactly
                    // the LIST area's size (GeometryReader-measured, clipped,
                    // see SidebarWorkspaceCarousel's geometry contract),
                    // holding two absolutely positioned pages of the same
                    // list component — this live page and, while a gesture or
                    // slide is in flight, the neighbor's pooled page. Both
                    // translate together via pure x-offsets; nothing a page
                    // renders can change the region's size, so the footer and
                    // its dots below stay put by construction. The carousel
                    // is a tiny modifier observing the per-event gesture
                    // state, so the heavy tree is never re-diffed per scroll
                    // event.
                    SidebarListPage(source: .live, store: store) {
                        projectTreePanel
                    }
                    .modifier(SidebarWorkspaceCarousel(
                        pager: workspacePager,
                        store: store
                    ))
                    .transition(.sidebarPanel(fromTrailing: false))
                }
            }
            .animation(SidebarMotion.slide, value: store.settingsVisible)
            .frame(maxHeight: .infinity)
            // Clips the settings-nav slide; the carousel additionally clips
            // its own fixed-frame region, so a translated page can never
            // paint outside the list area regardless of this outer clip.
            .clipped()
            // Programmatic single-slide switches (footer dot / selector /
            // settings clicks) reuse this pager. They originate outside this
            // view, so it registers the pager on the store and keeps the
            // pager's width current (a click has no gesture to capture one).
            .onGeometryChange(for: CGFloat.self) { proxy in
                proxy.size.width
            } action: { width in
                workspacePager.noteWidth(width)
            }
            .onAppear {
                store.workspacePagerAnimator = workspacePager
                // The detached session drag must never arm against a
                // translated carousel: registered row frames live in the
                // page's untranslated content space.
                sessionDragController.isSurfaceBusy = { [weak workspacePager] in
                    workspacePager?.isTranslated ?? false
                }
            }
            .onDrop(of: [.fileURL], isTargeted: $folderDropTargeted) { providers in
                guard store.selectedHostScope.isLocalMachine else { return false }
                dragState.folderHoverReset()
                return store.addProjectFolders(from: providers)
            }
            // Catch-all delegate over the whole list area: commits the final
            // session preview on drop anywhere (the visual order was already
            // applied by row-level dropEntered moves) and keeps the cursor on the
            // `.move` proposal in row gaps / empty space — the closure-based
            // onDrop used here before let AppKit fall back to `.copy`,
            // which showed the green "+" badge between rows.
            .onDrop(of: [.plainText], delegate: SidebarContainerDropDelegate(
                isDragActive: { [weak dragState] in dragState?.isActive ?? false },
                finish: { [weak dragState] in dragState?.finish() }
            ))
            .overlay {
                // Highlight whether the folder hovers empty list space (parent
                // onDrop `folderDropTargeted`) or directly over a row (the row
                // delegates' hover counter) — both mean "drop to add project".
                // A separate observing view: the row hover counter's churn
                // during a Finder drag wakes only this overlay, never the
                // heavy tree body above.
                SidebarFolderDropHighlight(
                    dragState: dragState,
                    externallyTargeted: folderDropTargeted
                )
            }
            // The button chrome hides while Settings owns the sidebar, but
            // workspace identity does not: keep the dot switcher mounted in
            // the same bottom slot so users can change the active workspace
            // from Settings too. The fixed-height shell prevents the settings
            // transition from moving the dots or the list above them.
            ZStack {
                if !store.settingsVisible {
                    SidebarFooter(
                        localVerbsVisible: store.selectedHostScope.isLocalMachine,
                        onAddProject: { store.addProjectFolder() },
                        onOpenSettings: { store.openSettings() },
                        onAddWorkspace: { addWorkspaceSheetMethod = .thisMac }
                    )
                    .transition(.opacity)
                }
            }
            .frame(maxWidth: .infinity)
            .frame(height: 31.5)
            // Dia-style workspace dots: a centered layer aligned to the
            // footer's 22pt button row (plus 9.5pt bottom inset). Hidden until
            // there is more than one workspace. Click switches; secondary-
            // click opens the complete workspace picker.
            .overlay {
                SidebarWorkspaceDots(
                    store: store,
                    pager: workspacePager,
                    pool: store.workspacePool
                )
                .padding(.bottom, 9.5)
            }
            // The same sheet Settings ▸ Workspaces presents; the normal
            // footer's "+" popover opens it on the local tab.
            .sheet(item: $addWorkspaceSheetMethod) { method in
                AddWorkspaceSheet(
                    store: store,
                    hosts: store.remoteHostStore,
                    initialMethod: method
                )
            }
        }
        // Animate the footer's hide/show alongside the pane slide. Scoped to
        // this sidebar VStack only — the content pane's Metal-backed terminal
        // lives outside it, so this stays a sidebar-local transaction.
        .animation(SidebarMotion.slide, value: store.settingsVisible)
        // The trackpad monitor drives the workspace carousel (the container
        // modifier on the list area above): same gating as the sidebar dots,
        // and inert while the settings nav occupies
        // the list area. The invisible monitor view never hit-tests; rows
        // are built once per gesture at engagement, and gesture end either
        // commits through the shared WorkspaceSwitching.select or springs
        // back. Reduce Motion and legacy phase-less wheels keep the
        // instantaneous cycle.
        .background {
            if WorkspaceFeature.pickerEnabled,
               !store.settingsVisible {
                SidebarWorkspaceSwipeMonitor(
                    state: workspacePager,
                    rows: {
                        // Gesture engagement is a pool immediate-refresh
                        // trigger (throttled inside): the peeked neighbor's
                        // snapshot should be as fresh as possible.
                        store.workspacePool.requestImmediateRefresh()
                        return WorkspaceSwitching.orderedRows(store: store)
                    },
                    isScoped: { WorkspaceSwitching.isScoped($0, store: store) },
                    select: { WorkspaceSwitching.select($0, store: store) },
                    cycle: { WorkspaceSwitching.cycle(by: $0, store: store) }
                )
            }
        }
        // Fixed top drag chrome stays outside the sliding ZStack so it remains
        // stable as the list area slides between the project tree, worktrees
        // view and settings nav. It is visually transparent; row dissolution
        // is owned by `SidebarListFadeMask`.
        .overlay(alignment: .top) {
            SidebarTopGlassOverlay()
        }
    }

    private var projectTreePanel: some View {
        ScrollViewReader { proxy in
            ScrollView {
                LazyVStack(spacing: 1) {
                    // This reads the workspace registry's modification stamp.
                    // Resolve it once for the tree instead of once per top-level
                    // project whenever an unrelated store publication redraws
                    // the sidebar.
                    let workspaceMoveTargets = store.localWorkspaceMoveTargets()
                    // Workspace switching lives in the footer dots, their
                    // secondary-click popover, and the trackpad swipe.
                    ForEach(store.displayNodes) { node in
                        ProjectNodeView(
                            store: store,
                            dragState: dragState,
                            node: node,
                            depth: 0,
                            workspaceMoveTargets: workspaceMoveTargets
                        )
                    }
                }
                // Keep rows entering the fixed top chrome inside the fade ramp,
                // so they dissolve before reaching the titlebar controls. The
                // top inset is the SHARED SidebarListPageMetrics constant both
                // carousel pages carry — the pooled page must start its rows
                // at exactly this y or the commit swap would jump. The
                // bottom padding must cover SessionScrollTarget.margin, or
                // auto-scrolls to the last rows clamp back under the bottom
                // fade.
                .padding(EdgeInsets(
                    top: SidebarListPageMetrics.topInset, leading: 8,
                    bottom: SessionScrollTarget.margin + 12, trailing: 8
                ))
                // Absolute-top anchor: attached OUTSIDE the padding (marker
                // top == the padded content's very top == content offset 0),
                // so `scrollTo(anchor: .top)` rests the tree exactly at its
                // natural top — the frame the carousel's pooled page renders
                // from — never above it and never scrolled 48pt under the
                // glass (which is what an anchor INSIDE the padding would
                // do).
                .background(alignment: .top) {
                    Color.clear
                        .frame(height: 1)
                        .id(Self.treeTopScrollID)
                }
                // While a session drag is in flight the list stops
                // hit-testing, so row hover chrome can't churn under the
                // floating card (the monitor is a local event monitor — it
                // keeps receiving the drag regardless). The toggle lives in
                // its own observing modifier BELOW the monitor background so
                // (a) the toggle re-runs only the modifier body, and (b) the
                // monitor's NSViewRepresentable sits OUTSIDE the toggled
                // subtree — applying `.allowsHitTesting` directly here
                // remounted the monitor view (and tore down its NSEvent
                // monitor) on every drag begin/end.
                .modifier(SidebarSessionDragHitTestGate(dragState: dragState))
                // Detached session drag: the shared content coordinate space
                // the row-frame registry measures in, and the invisible
                // NSEvent monitor view aligned to that same space (its
                // flipped AppKit coords match; the floating card itself is
                // an AppKit window owned by the controller).
                .coordinateSpace(name: SidebarSessionDragController.contentSpace)
                .background {
                    SidebarSessionDragMonitor(
                        controller: sessionDragController,
                        store: store,
                        dragState: dragState
                    )
                }
            }
            .scrollIndicators(.hidden)
            .mask(SidebarListFadeMask())
            .environment(\.sidebarSessionDragController, sessionDragController)
            .overlay {
                if store.displayNodes.isEmpty {
                    if store.selectedHostScope.isLocalMachine {
                        // A `.localWorkspace` is the same machine — offer Add
                        // Project against its scoped home.
                        SidebarEmptyProjectsView {
                            store.pickProjectFolder { _ in }
                        }
                    } else {
                        RemoteScopeEmptySidebarView(
                            hostName: store.remoteScopeDisplayName ?? "Remote Host",
                            state: store.remoteHostRuntime.connectionState,
                            hasLoadedSnapshot: store.remoteHostRuntime.snapshot != nil
                        )
                    }
                }
            }
            // The lifted row's floating card rides its own AppKit WINDOW
            // owned by the drag controller, not a panel-level overlay: an
            // overlay here would clip under the terminal the moment the card
            // crossed the sidebar edge (the sidebar slot in RootView is
            // `.clipped()`, and the content pane sits beside it), and a
            // SwiftUI-positioned card would trail the cursor by a
            // transaction commit.
            // ONE combined handler for scope + selection scrolling, with a
            // deterministic priority: a HOST-SCOPE SWITCH always lands the
            // tree at the TOP, unanimated, in the same commit the new scope's
            // rows appear (matching the carousel's pooled page, which always
            // renders from the top — that agreement is what makes the
            // carousel's commit swap pixel-identical; per-workspace
            // remembered offsets were rejected, see
            // SidebarWorkspaceDots.swift). The
            // selection a switch adopts in that same commit must NOT start
            // its animated reveal-scroll underneath — with two separate
            // onChange modifiers the outcome would depend on handler order.
            .overlay {
                SidebarSelectionScrollObserver(
                    selection: store.sessionSelection,
                    scope: store.selectedHostScope,
                    proxy: proxy,
                    treeTopScrollID: Self.treeTopScrollID,
                    sessionDragController: sessionDragController
                )
                .allowsHitTesting(false)
            }
            .onChange(of: store.sidebarSessionRevealRequest) { request in
                if let request {
                    scrollToSession(
                        request.sessionID, proxy: proxy,
                        anchor: request.centered ? .center : nil
                    )
                }
            }
            .onAppear {
                if let request = store.sidebarSessionRevealRequest {
                    scrollToSession(
                        request.sessionID, proxy: proxy,
                        anchor: request.centered ? .center : nil
                    )
                }
            }
        }
    }

    private func scrollToSession(
        _ sessionID: String,
        proxy: ScrollViewProxy,
        anchor: UnitPoint? = nil
    ) {
        DispatchQueue.main.async {
            withAnimation(.easeOut(duration: 0.22)) {
                proxy.scrollTo(SessionScrollTarget.id(sessionID), anchor: anchor)
            }
        }
    }
}

private struct SidebarEmptyProjectsView: View {
    let onAddProject: () -> Void

    var body: some View {
        VStack(spacing: 14) {
            ChromeIconView(icon: .folderClosed, size: 40)
                .foregroundStyle(Theme.foreground)

            Button {
                onAddProject()
            } label: {
                Label("Add Project", systemImage: "folder")
                    .font(.system(size: 13, weight: .semibold))
            }
            .buttonStyle(.borderedProminent)
            .tint(Theme.ctaTint)
        }
        .frame(maxWidth: .infinity, maxHeight: .infinity)
        .padding(.horizontal, 18)
        .padding(.top, Theme.titlebarHeight)
    }
}

/// The inputs that drive the project tree's programmatic scrolling, combined
/// into one Equatable value so a single onChange resolves their priority
/// (scope switch → top; selection change → reveal the row) without depending
/// on the ordering of separate change handlers inside one commit.
private struct SidebarScrollCue: Equatable {
    let scope: SelectedHostScope
    let selection: String?
}

/// The only global-selection observer inside the sidebar tree. Keeping this
/// subscription in a zero-content overlay means a selection publication can
/// drive programmatic scrolling without waking `SidebarView` or any project
/// node. Row highlights have their own per-id observers above.
private struct SidebarSelectionScrollObserver: View {
    @ObservedObject var selection: SessionSelectionState
    let scope: SelectedHostScope
    let proxy: ScrollViewProxy
    let treeTopScrollID: String
    let sessionDragController: SidebarSessionDragController

    @State private var previousCue: SidebarScrollCue?

    private var cue: SidebarScrollCue {
        SidebarScrollCue(scope: scope, selection: selection.sessionID)
    }

    var body: some View {
        Color.clear
            .onAppear { previousCue = cue }
            .onChange(of: cue) { newCue in
                let scopeChanged = previousCue?.scope != newCue.scope
                previousCue = newCue
                if scopeChanged {
                    var transaction = Transaction()
                    transaction.disablesAnimations = true
                    withTransaction(transaction) {
                        proxy.scrollTo(treeTopScrollID, anchor: .top)
                    }
                } else if let id = newCue.selection,
                          sessionDragController.sessionRowNeedsReveal(
                              id,
                              topOcclusion: Theme.titlebarHeight,
                              edgeMargin: SessionScrollTarget.margin
                          ) {
                    withAnimation(.easeOut(duration: 0.15)) {
                        proxy.scrollTo(SessionScrollTarget.id(id))
                    }
                }
            }
    }
}

/// Scroll-target geometry for session rows. Each row registers an invisible
/// scroll target that extends `margin` past the row on both ends, so a
/// nil-anchor (minimal) `scrollTo` stops with the row clear of the top chrome
/// veil and the bottom fade instead of flush against the sidebar edges.
enum SessionScrollTarget {
    static let margin: CGFloat = 48
    static func id(_ sessionID: String) -> String { "scroll-target:\(sessionID)" }
}

/// Vertical fade mask over the sidebar lists. The top stays faintly visible so
/// rows blur into the chrome instead of disappearing behind a hard transparent
/// cut.
struct SidebarListFadeMask: View {
    private static let topMinOpacity: CGFloat = 0
    private static let opaqueAt: CGFloat = 76

    var body: some View {
        VStack(spacing: 0) {
            LinearGradient(
                gradient: Gradient(stops: Self.topStops),
                startPoint: .top, endPoint: .bottom
            )
            .frame(height: Self.opaqueAt)
            Color.black
            LinearGradient(
                colors: [.black, .clear],
                startPoint: .top, endPoint: .bottom
            )
            .frame(height: 26)
        }
    }

    /// Gradient stops sample the smoothstep densely enough to stay smooth
    /// (SwiftUI interpolates linearly between stops).
    private static let topStops: [Gradient.Stop] = {
        let steps = 8
        var stops: [Gradient.Stop] = [
            .init(color: .black.opacity(topMinOpacity), location: 0)
        ]
        for step in 0 ... steps {
            let t = CGFloat(step) / CGFloat(steps)
            let alpha = topMinOpacity + (1 - topMinOpacity) * smoothstep(t)
            stops.append(.init(
                color: .black.opacity(alpha),
                location: t
            ))
        }
        return stops
    }()
}

private struct SidebarTopGlassOverlay: View {
    var body: some View {
        WindowDragArea()
            .frame(height: Theme.titlebarHeight)
    }
}

/// Hermite smoothstep: zero first derivative at both ends, so gradient
/// ramps built from it start and finish without visible edges.
private func smoothstep(_ t: CGFloat) -> CGFloat {
    let x = min(max(t, 0), 1)
    return x * x * (3 - 2 * x)
}

/// Text-glyph chevron (›/‹) vertically centered on its INK, not its line
/// box. Guillemets sit on the x-height midline, so inside an HStack(.center)
/// the bare Text lands ~1pt below the row's visual centerline (badges and
/// labels centered on cap height) — measured 2.5 device px off in the
/// worktrees link row. The offset is computed once per (glyph, size) from
/// the CoreText ink bounds, so it tracks the system font's real metrics.
struct ChevronGlyph: View {
    let glyph: String
    let size: CGFloat

    var body: some View {
        Text(glyph)
            .font(.system(size: size))
            .offset(y: Self.inkCenterOffset(glyph: glyph, size: size))
    }

    /// +down offset that moves the glyph's ink center onto the line-box
    /// center: inkRect.midY − (ascender + descender)/2, both baseline-rel.
    @MainActor private static var cache: [String: CGFloat] = [:]

    @MainActor
    static func inkCenterOffset(glyph: String, size: CGFloat) -> CGFloat {
        let key = "\(glyph)@\(size)"
        if let cached = cache[key] { return cached }
        var offset: CGFloat = 0
        let font = NSFont.systemFont(ofSize: size)
        var chars = Array(glyph.utf16)
        var glyphs = [CGGlyph](repeating: 0, count: chars.count)
        if CTFontGetGlyphsForCharacters(font, &chars, &glyphs, chars.count),
           let first = glyphs.first {
            var g = first
            let ink = CTFontGetBoundingRectsForGlyphs(font, .default, &g, nil, 1)
            let frameCenter = (font.ascender + font.descender) / 2
            offset = ink.midY - frameCenter
        }
        cache[key] = offset
        return offset
    }
}

// MARK: - Project node (project row + sessions + worktree children)

extension ProjectNode {
    /// Whether this folder owns the selected Session directly or through a
    /// nested group/worktree. Inline folder shells use this to retain their
    /// hover wash while one of their descendants is active.
    func containsSidebarSession(_ sessionID: String?) -> Bool {
        guard let sessionID else { return false }
        return sessions.contains { $0.id == sessionID }
            || worktrees.contains { $0.containsSidebarSession(sessionID) }
    }
}

/// A destination in the session context menu's "Move to" flyout.
struct SessionMoveTarget: Identifiable, Equatable {
    let id: String
    let name: String
}

/// `SessionRowView` carries a couple of dozen action closures, which SwiftUI
/// cannot compare — so without this every store publish (any session's hook
/// activity) re-ran every row body and rebuilt any open context menu, and
/// the "Move to" flyout blinked under the pointer. Compare the value inputs
/// only (closures never change what the row shows), and place the row with
/// `.equatable()` so unchanged rows skip their body entirely.
extension SessionRowView: @MainActor Equatable {
    static func == (lhs: SessionRowView, rhs: SessionRowView) -> Bool {
        lhs.session == rhs.session
            && lhs.depth == rhs.depth
            && lhs.indentBase == rhs.indentBase
            && lhs.isSelected == rhs.isSelected
            && lhs.selectionState === rhs.selectionState
            && lhs.shortcutHint == rhs.shortcutHint
            && lhs.isUnread == rhs.isUnread
            && lhs.isPinned == rhs.isPinned
            && lhs.canPin == rhs.canPin
            && lhs.isConfirmingRemove == rhs.isConfirmingRemove
            && lhs.isConfirmingArchive == rhs.isConfirmingArchive
            && lhs.isRemoving == rhs.isRemoving
            && lhs.isRestarting == rhs.isRestarting
            && lhs.isResumingAgent == rhs.isResumingAgent
            && lhs.isArchiving == rhs.isArchiving
            && lhs.isArchived == rhs.isArchived
            && lhs.isEditing == rhs.isEditing
            && lhs.notifyWhenDone == rhs.notifyWhenDone
            && lhs.canRestart == rhs.canRestart
            && lhs.canResumeAgent == rhs.canResumeAgent
            && lhs.canArchive == rhs.canArchive
            && lhs.canNotifyWhenDone == rhs.canNotifyWhenDone
            && lhs.canClearAttention == rhs.canClearAttention
            && lhs.paneItems == rhs.paneItems
            && lhs.ageTimestampMs == rhs.ageTimestampMs
            && lhs.moveTargets == rhs.moveTargets
            && (lhs.onMoveToProjectSidebar == nil) == (rhs.onMoveToProjectSidebar == nil)
            && (lhs.onMoveToMainArea == nil) == (rhs.onMoveToMainArea == nil)
    }
}

/// Value-equality gate for the project row (same reason as
/// `SessionRowView`): store publishes re-evaluate the tree many times a
/// second, and the row's closures defeat SwiftUI's structural diff, so an
/// open context-menu flyout (Move to ▸ …, Open in ▸ …) was torn down and
/// rebuilt on every tick. Closures compare by presence only.
extension ProjectRowView: @MainActor Equatable {
    static func == (lhs: ProjectRowView, rhs: ProjectRowView) -> Bool {
        lhs.node == rhs.node
            && lhs.depth == rhs.depth
            && lhs.isExpanded == rhs.isExpanded
            && lhs.showsAttentionDot == rhs.showsAttentionDot
            && lhs.showsUnreadDot == rhs.showsUnreadDot
            && lhs.showsBusyShimmer == rhs.showsBusyShimmer
            && lhs.quickGroups == rhs.quickGroups
            && lhs.menuPresets == rhs.menuPresets
            && lhs.addableApps == rhs.addableApps
            && (lhs.onAddApp == nil) == (rhs.onAddApp == nil)
            && lhs.folderColor == rhs.folderColor
            && lhs.showsWorktreeCreate == rhs.showsWorktreeCreate
            && lhs.showsLocalProjectVerbs == rhs.showsLocalProjectVerbs
            && lhs.showsOrganizationVerbs == rhs.showsOrganizationVerbs
            && lhs.showsFilesystemVerbs == rhs.showsFilesystemVerbs
            && lhs.isConfirmingRemove == rhs.isConfirmingRemove
            && lhs.shortcutHint == rhs.shortcutHint
            && lhs.hasLiveSessions == rhs.hasLiveSessions
            && lhs.archivedSessionCount == rhs.archivedSessionCount
            && lhs.localSiteURLs == rhs.localSiteURLs
            && lhs.isPinned == rhs.isPinned
            && lhs.canPin == rhs.canPin
            && lhs.isDateSorted == rhs.isDateSorted
            && lhs.workspaceMoveTargets == rhs.workspaceMoveTargets
    }
}

struct ProjectNodeView: View {
    @ObservedObject var store: UnpeelStore
    /// Plain reference, NOT observed (the old `@EnvironmentObject` here made
    /// every drag-state publish re-evaluate every project node — the whole
    /// heavy tree — per hover change during a drag). The drag-driven row
    /// effects live in the small observing modifiers below
    /// (`SidebarProjectRowDragEffects` / `SidebarSessionRowDragEffects`), so
    /// only those leaf bodies re-run.
    let dragState: SidebarDragState
    let node: ProjectNode
    let depth: Int
    /// Resolved once by the root tree and threaded through child folders.
    /// Only top-level project menus consume it.
    let workspaceMoveTargets: [WorkspaceMoveTarget]

    private var isExpanded: Bool { store.expandedProjectIDs.contains(node.id) }

    /// Group or worktree — inline child-folder row with a leading disclosure
    /// chevron and a wash behind the whole cluster on hover.
    private var isInlineFolder: Bool {
        node.project.isFolder == true
            || node.project.parentProjectID != nil
            || node.project.worktreeBranch != nil
    }

    /// Hover wash behind the whole cluster (header + sessions), not just
    /// the title row.
    @State private var groupHovering = false

    /// Top-level projects and child folders both use the detached drag;
    /// children interleave with sessions, while top-level projects reorder
    /// only among their siblings.
    private var isProjectDraggable: Bool { node.project.parentProjectID == nil }

    // Whether an in-flight session drag may drop on a project row is decided
    // by the detached-drag controller's hit-test through the SAME store
    // predicate (`canMoveSession`) that mirrors `moveSession`'s own guards —
    // a target that highlights always moves. This row only renders the
    // highlight (`SidebarProjectRowDragEffects`) and registers its frame.

    /// Pinned/regular/displayed lists live on the store now. The rendered
    /// lists are parent-ordered so ⌘1–9 shortcut targets match the visual
    /// rows. The pinned partition is ONE mixed list: pinned sessions and
    /// pinned child groups interleave in the user's order.
    private var renderedPinnedItems: [SidebarMixedItem] {
        store.renderedPinnedItems(in: node)
    }

    private var archivedSessionCount: Int {
        store.archivedSessionCount(in: node)
    }

    /// Collapsed-project state rollup over this project's sessions plus
    /// worktree descendants. Precedence attention > unread mirrors the
    /// `project-state-dot` markup (ProjectItem.svelte:1228-1232).
    private var aggregateHasAttention: Bool {
        func check(_ node: ProjectNode) -> Bool {
            node.sessions.contains { $0.status == .attention }
                || node.worktrees.contains(where: check)
        }
        return check(node)
    }

    /// projectHasUnread (ProjectItem.svelte:352-354) incl. worktree children.
    private var aggregateHasUnread: Bool {
        func check(_ node: ProjectNode) -> Bool {
            node.sessions.contains { store.sessionIsUnread($0.id) }
                || node.worktrees.contains(where: check)
        }
        return check(node)
    }

    var body: some View {
        if isInlineFolder {
            groupCluster
                // "Files into this group" wash while an accepted session
                // drag hovers the header OR the expanded shell (both set
                // this node as the drop target).
                .modifier(SidebarProjectRowDragEffects(
                    dragState: dragState, nodeID: node.id
                ))
                // Same-section transform gap: the WHOLE cluster (header +
                // expanded contents + wash) rides as one unit. The shift
                // must never sit on the header row alone — that tore the
                // header out of its block, leaving empty space inside and
                // below the group.
                .modifier(SidebarFolderClusterDragEffects(
                    dragState: dragState,
                    folderID: node.id,
                    parentID: node.project.parentProjectID ?? ""
                ))
        } else {
            VStack(alignment: .leading, spacing: 0) {
                projectRow
                if isExpanded {
                    sessionList
                        .transition(.sessionListContent)
                }
            }
            // The header remains the only project-drag SOURCE. This larger
            // overlapping record makes the whole expanded block a reliable
            // top-level reorder TARGET even when the pointer is over one of
            // its Session/group descendants.
            .background {
                Color.clear.sidebarDragRowFrame(
                    id: "project-shell-drop:\(node.id)",
                    kind: .projectShell(projectID: node.id)
                )
            }
            .modifier(SidebarProjectBlockDragEffects(
                dragState: dragState, nodeID: node.id
            ))
        }
    }

    /// Header + sessions as one hover target so a muted wash can sit
    /// behind the whole group or worktree.
    private var groupCluster: some View {
        VStack(alignment: .leading, spacing: 0) {
            projectRow
            if isExpanded {
                sessionList
                    .transition(.sessionListContent)
            }
        }
        .background {
            GroupClusterBackground(
                selection: store.sessionSelection,
                node: node,
                isHovering: groupHovering
            )
        }
        .background {
            // Drop-target frame lives on its OWN stable layer, not on the
            // hover-animated wash above. A group whose sessions are all
            // stopped/archived (so its body is nearly empty) would otherwise
            // report a small, animation-jittery shell frame — sometimes stale
            // under the cursor — and a dragged session would fall through to
            // the parent project shell instead of filing INTO the group. This
            // matches the .projectShell registration on the non-inline branch.
            Color.clear.sidebarDragRowFrame(
                id: "group-shell-drop:\(node.id)",
                kind: .groupShell(projectID: node.id)
            )
        }
        .onHover { groupHovering = $0 }
    }

    /// Busy shimmer on the project NAME (ProjectItem.svelte:1268,
    /// `class:shimmer={aggregateState === 'busy'}`). Rolls up over the OWN
    /// sessions always, and over worktree descendants while collapsed —
    /// matching the attention/unread dots, so a busy session hidden inside
    /// a folded subtree still reads as activity on the visible row.
    /// Attention outranks busy, so attention suppresses the shimmer.
    private var showsBusyShimmer: Bool {
        func busy(_ n: ProjectNode) -> Bool {
            n.sessions.contains { $0.status == .starting || $0.status == .busy }
                || n.worktrees.contains(where: busy)
        }
        let busyNow = node.sessions.contains { $0.status == .starting || $0.status == .busy }
            || (!isExpanded && node.worktrees.contains(where: busy))
        return !aggregateHasAttention && busyNow
    }

    @ViewBuilder
    private var projectRow: some View {
        // `showsLocalProjectVerbs` is the pure-`.local` gate: native preset
        // management (Settings ▸ Agents & Apps edits THIS instance's home).
        // `isLocalMachine` gates the verbs that ARE valid against a scoped
        // local workspace on this Mac: organization verbs (folder color, sort,
        // groups, rename — Host carriers or the scoped home's own records),
        // filesystem verbs (Reveal/Open in editor operate on the real path),
        // and Remove/Add (they run local-against the scoped workspace's home).
        // A true remote Host scope hides all of them.
        let isLocalScope = store.selectedHostScope == .local
        let isLocalMachine = store.selectedHostScope.isLocalMachine
        let worktreesEnabled = store.isExperimentalEnabled(.worktrees)
        // Local machine, not just Local scope: a scoped workspace's project
        // paths are real paths on this Mac, so git worktree verbs are valid
        // there too (the store routes their session/record halves per scope).
        let isGitProject = isLocalMachine
            && node.project.isFolder != true
            && UnpeelStore.isGitRepo(path: node.project.path)
        let canCreateWorktree = worktreesEnabled
            && isGitProject && node.project.worktreeBranch == nil
        let row = ProjectRowView(
            node: node,
            depth: depth,
            isExpanded: isExpanded,
            showsAttentionDot: !isExpanded && aggregateHasAttention,
            showsUnreadDot: !isExpanded && aggregateHasUnread,
            showsBusyShimmer: showsBusyShimmer,
            quickGroups: store.displayQuickPresetGroups,
            menuPresets: store.displayAvailablePresets,
            addableApps: isLocalScope ? store.addableApps : [],
            onAddApp: isLocalScope ? { store.addAppPreset($0) } : nil,
            folderColor: isLocalMachine ? store.projectFolderColor(for: node.id) : nil,
            // Worktree menu toggle gate (ProjectItem.svelte:843-848):
            // real project (not a plain folder), not a worktree child,
            // and the path is a git repo.
            showsWorktreeCreate: canCreateWorktree,
            showsLocalProjectVerbs: isLocalScope,
            showsOrganizationVerbs: isLocalMachine,
            showsFilesystemVerbs: isLocalMachine,
            isConfirmingRemove: store.confirmingRemoveProjectID == node.id,
            shortcutHint: store.projectShortcutHintIndex(forProject: node.id),
            hasLiveSessions: node.sessions.contains(where: \.isLive),
            archivedSessionCount: archivedSessionCount,
            // Host-probed live URLs are family-wide, so only the top-level
            // project row wears the globe — child groups/worktrees would
            // repeat the same links.
            localSiteURLs: isLocalScope && node.project.parentProjectID == nil
                ? store.localSiteURLs(forProjectFamilyOf: node.id)
                : [],
            // Top-level projects reorder among projects; child groups and
            // worktrees already interleave with sessions through the same
            // detached controller. Every reorderable project row therefore
            // advertises the same anywhere-on-row interaction.
            onToggle: toggleExpansion,
            onLaunchPreset: { preset in
                // startSessionOrToast via launchPresetForProject
                // (App.svelte:1168-1175): command = preset command,
                // label = command for non-blank / "Terminal" for blank —
                // which is exactly what launchSession derives.
                store.launchSession(
                    projectID: node.id,
                    command: preset.command,
                    sourcePresetID: preset.command.isEmpty ? nil : preset.id
                )
            },
            onCreateWorktree: { store.promptCreateWorktree(projectID: node.id) },
            onCreateGroup: { store.promptCreateGroup(projectID: node.id) },
            onRemoveGroup: { store.removeGroupProject(node.id) },
            onStopAll: { store.stopAllSessions(projectID: node.id) },
            onOpenArchived: { store.openArchivedSessions(projectID: node.id) },
            onRevealInFinder: { store.revealInFinder(path: node.project.path) },
            onOpenIn: { target in
                store.openWorkspace(path: node.project.path, in: target)
            },
            onRequestRemove: { store.requestRemoveProject(node.id) },
            onConfirmRemove: { store.removeProject(node.id) },
            onCancelRemove: { store.cancelRemoveProjectConfirm() },
            onRenameWorktree: { store.promptRenameWorktreeProject(node.id) },
            onRemoveWorktree: { store.removeWorktreeProject(node.id) },
            onSetFolderColor: { color in
                store.setProjectFolderColor(color, for: node.id)
            },
            isPinned: store.isGroupPinned(node.id),
            canPin: store.groupCanPin(node.id),
            onSetPinned: { pinned in
                store.setGroupPinned(node.id, pinned: pinned)
            },
            isDateSorted: store.isDateSorted(projectID: node.id),
            onSetSessionDateSorted: { dateSorted in
                store.setSessionDateSorted(dateSorted, for: node.id)
            },
            onManagePresets: { store.openSettings(tab: .presets) },
            workspaceMoveTargets: node.project.parentProjectID == nil
                && isLocalMachine
                ? workspaceMoveTargets
                : [],
            onMoveToWorkspace: { dest in
                store.moveProjectToLocalWorkspace(projectID: node.id, dest: dest)
            }
        ).equatable()

        if isProjectDraggable {
            row
                // Source-row dim during a project drag + the session-drag
                // drop-target highlight, in one small observing modifier so
                // drag-state churn never re-evaluates this node's body.
                .modifier(SidebarProjectRowDragEffects(
                    dragState: dragState, nodeID: node.id
                ))
                // Project reordering is handled by the same detached drag
                // controller as sessions. This drop delegate now only keeps
                // Finder folder drops working directly over the row.
                .onDrop(of: [.fileURL], delegate: SidebarReorderDropDelegate(
                    isReorderActive: { false },
                    moveOver: { false },
                    finishReorder: {},
                    setFolderHover: { [weak dragState] hovering in
                        hovering ? dragState?.folderHoverEnter() : dragState?.folderHoverExit()
                    },
                    addFolders: { [weak store, weak dragState] providers in
                        dragState?.folderHoverReset()
                        return store?.addProjectFolders(from: providers) ?? false
                    }
                ))
                // Group/root move target for the detached session drag: the
                // controller hit-tests this frame and gates the highlight +
                // drop through `UnpeelStore.canMoveSession`.
                .sidebarDragRowFrame(id: node.id, kind: .project)
        } else if let parentID = node.project.parentProjectID {
            row
                .modifier(SidebarSessionRowDragEffects(
                    dragState: dragState,
                    sessionID: node.id,
                    projectID: parentID,
                    // The whole cluster shifts as one unit (see the
                    // SidebarFolderClusterDragEffects on `groupCluster`);
                    // shifting the header here too would double-move it.
                    shiftsWithReorder: false
                ))
                .sidebarDragRowFrame(
                    id: node.id,
                    kind: .folderItem(
                        parentID: parentID,
                        pinned: store.isGroupPinned(node.id),
                        depth: depth
                    )
                )
                .onDrop(of: [.fileURL], delegate: SidebarReorderDropDelegate(
                    isReorderActive: { false },
                    moveOver: { false },
                    finishReorder: {},
                    setFolderHover: { [weak dragState] hovering in
                        hovering ? dragState?.folderHoverEnter() : dragState?.folderHoverExit()
                    },
                    addFolders: { [weak store, weak dragState] providers in
                        dragState?.folderHoverReset()
                        return store?.addProjectFolders(from: providers) ?? false
                    }
                ))
        } else {
            row
        }
    }

    /// Whole-row click toggles expansion (ProjectItem.svelte:654-659);
    /// open 340ms cubic-bezier(0.16,1,0.3,1), close 240ms cubicInOut.
    private func toggleExpansion() {
        let opening = !isExpanded
        withAnimation(opening ? SidebarMotion.accordionOpen : SidebarMotion.accordionClose) {
            store.toggleProjectExpanded(node.id)
        }
    }

    /// One container so the accordion content transition applies to the
    /// whole block, like .session-list in ProjectItem.svelte (worktree
    /// child rows live inside the same shell there too).
    private var sessionList: some View {
        // The outer project tree is lazy, but each expanded project is one of
        // its children. A regular VStack here therefore constructed every
        // Session row in that project at once. Keep the hierarchy/accordion
        // behavior while virtualizing rows inside large expanded projects.
        LazyVStack(alignment: .leading, spacing: 0) {
            // ⌘N hints while ⌘ is held and this is the shortcut project
            // (empty otherwise).
            let shortcutHints = store.sessionShortcutHintIndices(forProject: node.id)

            // Pane-group hiding: while a group is open, each non-representative
            // member leaves the list; closing the group fades it back into its
            // own spot. Animation lives HERE (not on the store mutation)
            // so the Metal terminal does not inherit a fade.
            let hiddenPaneMemberIDs = store.activePaneMemberSessionIDs

            // Pinned partition: pinned sessions AND pinned child groups in
            // one mixed list, ordered by the project's pin records — a
            // pinned group can sit between two pinned sessions.
            let pinnedItems = renderedPinnedItems
            ForEach(Array(pinnedItems.enumerated()), id: \.element.id) { index, item in
                switch item {
                case .folder(let child):
                    ProjectNodeView(
                        store: store,
                        dragState: dragState,
                        node: child,
                        depth: depth + 1,
                        workspaceMoveTargets: workspaceMoveTargets
                    )
                    .transition(.sessionRowStagger(index: index))
                case .session(let session):
                    if !hiddenPaneMemberIDs.contains(session.id) {
                        sessionRow(
                            session,
                            staggerIndex: index,
                            pinnedRow: true,
                            shortcutHint: shortcutHints[session.id]
                        )
                    }
                }
            }

            // Regular section: sessions AND child folders in one list so a
            // group can sit between any two sessions. Folders stay on top
            // until a drag writes them into session-order.json.
            let mixedItems = store.renderedDisplayedItems(in: node)
            ForEach(Array(mixedItems.enumerated()), id: \.element.id) { index, item in
                switch item {
                case .folder(let child):
                    ProjectNodeView(
                        store: store,
                        dragState: dragState,
                        node: child,
                        depth: depth + 1,
                        workspaceMoveTargets: workspaceMoveTargets
                    )
                    .transition(.sessionRowStagger(index: pinnedItems.count + index))
                case .session(let session):
                    if !hiddenPaneMemberIDs.contains(session.id) {
                        sessionRow(
                            session,
                            staggerIndex: pinnedItems.count + index,
                            pinnedRow: false,
                            shortcutHint: shortcutHints[session.id]
                        )
                    }
                }
            }

            // Empty state: archived sessions live in the main pane, so a
            // project with only archived history still reads as empty here.
            if pinnedItems.isEmpty && mixedItems.isEmpty {
                EmptySessionsPlaceholderRow(
                    label: archivedSessionCount == 0 ? "No sessions yet." : "No active sessions.",
                    depth: depth,
                    indentBase: 28,
                    menuPresets: store.displayAvailablePresets,
                    onLaunch: { preset in
                        store.launchSession(
                            projectID: node.id,
                            command: preset.command,
                            sourcePresetID: preset.command.isEmpty ? nil : preset.id
                        )
                    },
                    onManagePresets: { store.openSettings(tab: .presets) },
                    showsManagePresets: store.selectedHostScope == .local,
                    archivedCount: archivedSessionCount,
                    onOpenArchived: { store.openArchivedSessions(projectID: node.id) },
                    addableApps: store.selectedHostScope == .local ? store.addableApps : [],
                    onAddApp: store.selectedHostScope == .local ? { store.addAppPreset($0) } : nil
                )
            }

            // Everything past the inactive-preview window leaves the sidebar.
            // Hidden natural stops remain available through search/history;
            // hidden archives also live in the project context menu's
            // "Archived (N)" library — no sidebar footer row.
        }
        // Rows keep stable ids across the active → archive move, so a
        // just-archived session glides into the fixed bottom section instead
        // of teleporting. Suppressed from lift until one tick AFTER a drag
        // lands (`listGlideSuppressed`): the drop applies its reorder in an
        // animation-disabled transaction (rows are already visually in
        // place via the transform gap), but a value-keyed animation
        // OVERRIDES that transaction — it replayed the swap as a bounce
        // from the old order to the drop position. The flag outlives the
        // land transaction on purpose; everything cleared inside it (drag
        // state, lifted row) reads as already-idle by the commit render.
        .animation(
            dragState.listGlideSuppressed ? nil : SidebarMotion.accordionOpen,
            value: store.renderedPinnedItems(in: node).map(\.id)
                + store.renderedDisplayedItems(in: node).map(\.id)
        )
        .animation(
            .easeOut(duration: 0.28),
            value: store.activePaneMemberSessionIDs
        )
    }

    @ViewBuilder
    private func sessionRow(
        _ session: SessionEntry,
        staggerIndex: Int,
        pinnedRow: Bool,
        shortcutHint: Int? = nil
    ) -> some View {
        // A date-sorted group has no manual order to write, so re-ordering
        // is off entirely.
        let isReorderable = !store.isDateSorted(projectID: node.id)
        let isArchived = store.sessionIsRecentArchived(session.id)
        // Detached-drag participation is broader than reordering: a LOCAL
        // session in a date-sorted group must still lift, so it can be
        // dragged OUT of the group (a committed within-list reorder flips
        // the group to custom order — see commitSessionReorder). Host-backed
        // lists commit that flip through `project.organization.set`.
        //
        // Rename and inline remove-confirm must NOT drop this row from the
        // drag-frame registry. Swapping the wrapper off (and back) remounts
        // the row: the title editor blinks, focus-loss can immediately
        // commit, and `onGeometryChange` can skip a re-report when the
        // replacement lands on the same frame — leaving the row undraggable
        // until the project list rebuilds. The controller refuses to arm a
        // press while those modes own the pointer (text selection / confirm
        // buttons).
        let isDraggable = !isArchived && (
            isReorderable || store.canLiftSessionFromDateSortedList(session.id)
        )
        // Native-only within-project drag reorder (the Svelte app has no
        // within-project reorder). Session rows use the manual detached drag
        // (SidebarSessionDrag.swift): mouse-down anywhere on the row plus
        // ~6pt of travel lifts the row into a floating card; the registered
        // frame below is what the controller hit-tests. Pinned and regular
        // rows each reorder only within their own section — the controller
        // gates on `drag.pinned` exactly as the drop delegates used to.
        let row = SessionRowView(
            session: session,
            depth: depth,
            // Depth alone supplies the standard 14pt nesting step beneath a
            // child folder; no extra child-folder offset is needed.
            indentBase: 9,
            isSelected: false,
            selectionState: store.sessionSelection.rowState(for: session.id),
            shortcutHint: shortcutHint,
            isUnread: store.sessionIsUnread(session.id),
            isPinned: !isArchived
                && store.isPinned(sessionID: session.id, projectID: node.id),
            canPin: store.sessionCanPin(session.id),
            // Archive-page confirms render (and monitor click-away) on the
            // archive page only — a mirrored sidebar confirm would cancel
            // them on the mouse-down aimed at the archive card's buttons.
            isConfirmingRemove: store.confirmingRemoveSessionID == session.id
                && store.confirmingRemoveSurface == .sidebar,
            isConfirmingArchive: store.confirmingArchiveSessionID == session.id,
            isRemoving: store.removingSessionIDs.contains(session.id),
            isRestarting: store.restartingSessionIDs.contains(session.id),
            isResumingAgent: store.resumingAgentSessionIDs.contains(session.id),
            isArchiving: store.archivingSessionIDs.contains(session.id),
            isArchived: isArchived,
            isEditing: store.editingSessionID == session.id
                && store.editingSessionSurface == .sidebar,
            notifyWhenDone: store.notifyWhenDoneSessionIDs.contains(session.id),
            canRestart: store.sessionCanRestart(session.id),
            canResumeAgent: store.sessionCanResumeAgent(session.id),
            canArchive: store.sessionCanArchive(session.id),
            canNotifyWhenDone: store.sessionCanNotifyWhenDone(session.id),
            canClearAttention: store.sessionCanClearAttention(session.id),
            paneItems: store.paneSidebarItems(
                forRepresentative: session.id
            ),
            ageTimestampMs: store.isDateSorted(projectID: node.id)
                ? max(session.createdAt, session.lifecycleAtMs ?? 0)
                : nil,
            onSelect: {
                store.selectedSessionID = session.id
            },
            onFocusPane: { paneSessionID in
                store.requestTerminalPaneFocus(
                    representativeSessionID: session.id,
                    sessionID: paneSessionID
                )
            },
            onHoverIntent: { store.prewarmSession(session.id) },
            onSetNotifyWhenDone: {
                store.setNotifyWhenDone(session.id, enabled: $0)
            },
            onResume: { store.resumeAgentOrSession(session.id) },
            onClearAttention: { store.clearAttention(session.id) },
            onSetPinned: { pinned in
                if pinned {
                    store.pinSession(projectID: node.id, sessionID: session.id)
                } else {
                    store.unpinSession(projectID: node.id, sessionID: session.id)
                }
                store.followSessionRowInSidebar(session.id)
            },
            onRequestRemove: { store.requestRemoveSession(session.id) },
            onConfirmRemove: { store.confirmRemoveSession(session.id) },
            onCancelRemove: { store.cancelRemoveConfirm() },
            onArchive: { store.requestArchiveSession(session.id) },
            onUnarchive: {
                if store.sessionCanRestart(session.id) {
                    store.resumeArchivedSession(session.id)
                } else {
                    store.unarchiveSession(session.id)
                }
            },
            onConfirmArchive: { store.archiveSession(session.id) },
            onCancelArchive: { store.cancelArchiveConfirm() },
            onCopyTranscript: {
                store.copyTranscriptMarkdown($0, entries: $1)
            },
            onBeginEdit: {
                store.beginEditingSessionTitle(session.id, on: .sidebar)
            },
            onCommitRename: { store.renameSession(session.id, to: $0) },
            onEndEdit: {
                if store.editingSessionID == session.id {
                    store.editingSessionID = nil
                }
            },
            moveTargets: store.isExperimentalEnabled(.worktrees)
                ? store.moveDestinations(forSession: session.id)
                    .map { SessionMoveTarget(id: $0.id, name: $0.name) }
                : [],
            onMoveTo: { store.moveSession(session.id, toProjectID: $0) },
            onMoveToProjectSidebar: store.canMoveSessionToProjectSidebar(session.id)
                ? { store.moveSessionToProjectSidebar(session.id) }
                : nil,
            onMoveToMainArea: store.sessionIsInProjectSidebar(session.id)
                ? { store.moveSessionToMainArea(session.id) }
                : nil
        ).equatable()
        .id(session.id)
        .background {
            // The `.id()` inside is on the un-padded Color.clear, so the
            // registered frame is the row grown by `margin` on both ends —
            // scroll targets aim at this, not the bare row.
            Color.clear
                .id(SessionScrollTarget.id(session.id))
                .padding(.vertical, -SessionScrollTarget.margin)
        }
        .transition(.sessionRowStagger(index: staggerIndex))

        if isDraggable {
            row
                // Lifted-slot gap + the cross-group insertion gaps, in
                // one small observing modifier (see its doc) so drag-state
                // churn re-runs only that leaf body — never this row body.
                .modifier(SidebarSessionRowDragEffects(
                    dragState: dragState,
                    sessionID: session.id,
                    projectID: node.id
                ))
                // Frame registry for the detached drag's hit-testing (live
                // reorder preview + press detection).
                .sidebarDragRowFrame(
                    id: session.id,
                    kind: .session(
                        projectID: node.id, pinned: pinnedRow, depth: depth
                    ),
                    isDraggable: true
                )
                // Finder folder drops on a row still mean "add project"; the
                // session reorder itself no longer arrives as a system drop.
                .onDrop(of: [.fileURL], delegate: SidebarReorderDropDelegate(
                    isReorderActive: { false },
                    moveOver: { false },
                    finishReorder: {},
                    setFolderHover: { [weak dragState] hovering in
                        hovering ? dragState?.folderHoverEnter() : dragState?.folderHoverExit()
                    },
                    addFolders: { [weak store, weak dragState] providers in
                        dragState?.folderHoverReset()
                        return store?.addProjectFolders(from: providers) ?? false
                    }
                ))
        } else {
            // Non-reorderable rows still report their geometry to the shared
            // mouse-down monitor so selection has the same immediate feel in
            // date-sorted/remote/archive lists.
            row.sidebarDragRowFrame(
                id: session.id,
                kind: .session(
                    projectID: node.id, pinned: pinnedRow, depth: depth
                ),
                isDraggable: false
            )
        }
    }
}

/// Empty-project placeholder: a muted status row shown when an expanded
/// project has no active sessions. Clicking it opens the same new-session menu
/// as the project row's "+" (blank terminal + presets + Manage presets).
struct EmptySessionsPlaceholderRow: View {
    var label = "No sessions yet."
    let depth: Int
    /// 28 aligns with session labels; depth supplies nested indentation.
    var indentBase: CGFloat = 28
    let menuPresets: [Preset]
    let onLaunch: (Preset) -> Void
    var onManagePresets: () -> Void = {}
    var showsManagePresets = true
    var archivedCount = 0
    var onOpenArchived: (() -> Void)?
    var addableApps: [InstalledAppInfo] = []
    var onAddApp: ((InstalledAppInfo) -> Void)?

    @State private var hovering = false

    var body: some View {
        HStack(spacing: 0) {
            Menu {
                newSessionMenuContent(
                    menuPresets: menuPresets,
                    onLaunch: onLaunch,
                    onManagePresets: onManagePresets,
                    showsManagePresets: showsManagePresets,
                    archivedCount: archivedCount,
                    onOpenArchived: onOpenArchived,
                    addableApps: addableApps,
                    onAddApp: onAddApp
                )
            } label: {
                Text(label)
                    .font(.system(size: 11))
                    .foregroundStyle(
                        hovering
                            ? Theme.foreground
                            : Theme.mutedForeground.opacity(0.72)
                    )
                    .padding(EdgeInsets(top: 3, leading: 6, bottom: 3, trailing: 6))
                    .background(
                        RoundedRectangle(cornerRadius: 4, style: .continuous)
                            .fill(hovering ? Theme.foreground.opacity(0.08) : .clear)
                    )
            }
            .menuStyle(.borderlessButton)
            .menuIndicator(.hidden)
            .fixedSize()
            .background(HoverReporter { hovering = $0 })
            .animation(.easeInOut(duration: 0.12), value: hovering)
            Spacer(minLength: 0)
        }
        .padding(EdgeInsets(
            top: 4,
            leading: 28 + CGFloat(depth) * 14,
            bottom: 4,
            trailing: 0
        ))
    }
}

/// A selection switch publishes through `SessionSelectionState`, not the
/// whole store. Keeping this observer in the background leaf lets ancestor
/// group/worktree washes follow selection without rebuilding their rows.
private struct GroupClusterBackground: View {
    @ObservedObject var selection: SessionSelectionState
    let node: ProjectNode
    let isHovering: Bool

    private var isHighlighted: Bool {
        isHovering || node.containsSidebarSession(selection.sessionID)
    }

    var body: some View {
        RoundedRectangle(cornerRadius: 10, style: .continuous)
            .fill(isHighlighted ? Theme.hoverRow.opacity(0.5) : .clear)
            // Child-group layout is already inset by its parent list. Expand
            // the wash back toward the Session-row bounds. Its natural height
            // gives the shell one extra point above and below compared with
            // the previous vertically-inset shape.
            .padding(.trailing, -1)
            .animation(.easeOut(duration: 0.12), value: isHighlighted)
    }
}

// MARK: - Project row

struct ProjectRowView: View {
    let node: ProjectNode
    let depth: Int
    let isExpanded: Bool
    /// Collapsed-project state dot on the folder icon
    /// (ProjectItem.svelte:1228-1232): attention (#eab308) wins over unread
    /// (#60a5fa); both only show while the project is collapsed.
    var showsAttentionDot = false
    var showsUnreadDot = false
    /// Gradient sweep over the project NAME while a session inside is busy
    /// (.project-name.shimmer, ProjectItem.svelte:2083-2102).
    var showsBusyShimmer = false
    // (The session-drag drop-target highlight is applied OUTSIDE this row by
    // `SidebarProjectRowDragEffects`, so drag-state churn never re-evaluates
    // this row body.)
    /// Strip contents: starred presets grouped per CLI, flat-list order (the
    /// strip appends the blank-terminal chip itself).
    let quickGroups: [QuickPresetGroup]
    /// All enabled presets, backing the "+" new-session menu.
    let menuPresets: [Preset]
    /// Installed Apps not yet in the launch list — the "Apps you can add"
    /// section of the "+" menu. Empty outside local scope.
    var addableApps: [InstalledAppInfo] = []
    var onAddApp: ((InstalledAppInfo) -> Void)?
    /// Optional native-only tint for this project's glass folder glyph.
    var folderColor: ProjectFolderColor?
    /// Whether the context menu offers New worktree… (gate: not a folder,
    /// not a worktree child, is a git repo — ProjectItem.svelte:843-848).
    var showsWorktreeCreate = false
    /// Pure-`.local` gate: native preset management (Manage Agents & Apps…)
    /// edits this instance's own home, so it stays off in a scoped workspace.
    var showsLocalProjectVerbs = true
    /// Whether the organization verbs (Rename, Folder color, Sort sessions,
    /// New group…, Remove group/worktree) are offered. True in `.local` AND
    /// in a scoped `.localWorkspace`: folder color, sort, and group rename
    /// ride the Host's `project.organization.set`; group create/remove and
    /// worktree rename write the scoped home's own records. False for a
    /// true remote Host.
    var showsOrganizationVerbs = true
    /// Whether the filesystem/state verbs valid on THIS machine (Reveal in
    /// Finder, Open in editor, Remove) are offered. True in `.local` AND in a
    /// scoped `.localWorkspace` (same Mac). False for a true remote Host.
    var showsFilesystemVerbs = true
    /// Inline "Remove project?" confirm — the whole row swaps, same
    /// pattern as the session remove confirm.
    var isConfirmingRemove = false
    /// 1-based ⌃N hint while ⌃ is held (project mirror of the session
    /// rows' ⌘N hints; nil = hidden).
    var shortcutHint: Int?
    /// Any live session in THIS project (not worktree children) — gates
    /// "Stop all" (hasLive, ProjectItem.svelte:896).
    var hasLiveSessions = false
    /// Archived sessions owned by this exact project. Recent ones can remain
    /// in the inactive preview; a non-zero count adds the complete main-pane
    /// archive library to the project context menu.
    var archivedSessionCount = 0
    /// Live local-site URLs served by this project's family (host-probed).
    /// Non-empty shows the globe link button beside the name and the Links
    /// section of the context menu. Top-level local projects only.
    var localSiteURLs: [String] = []
    let onToggle: () -> Void
    let onLaunchPreset: (Preset) -> Void
    var onCreateWorktree: () -> Void = {}
    /// "New group…" — a plain organizational child folder (no git).
    var onCreateGroup: () -> Void = {}
    /// Group rows swap Remove for this: forget the group; its sessions
    /// fall back to the parent on the next scan.
    var onRemoveGroup: () -> Void = {}
    var onStopAll: () -> Void = {}
    var onOpenArchived: () -> Void = {}
    var onRevealInFinder: () -> Void = {}
    /// Open the project's path in an external app (context-menu "Open in").
    var onOpenIn: (WorkspaceOpenTarget) -> Void = { _ in }
    var onRequestRemove: () -> Void = {}
    var onConfirmRemove: () -> Void = {}
    var onCancelRemove: () -> Void = {}
    /// Worktree child rows swap Remove for this: confirm dialog +
    /// `git worktree remove` + forget the project.
    var onRenameWorktree: () -> Void = {}
    var onRemoveWorktree: () -> Void = {}
    var onSetFolderColor: (ProjectFolderColor?) -> Void = { _ in }
    /// Plain-group Pin/Unpin is context-menu-only. A pinned group keeps a
    /// passive indicator; an unpinned row never gains pin chrome on hover.
    var isPinned = false
    var canPin = false
    var onSetPinned: (Bool) -> Void = { _ in }
    /// Whether this group's sessions sort by date (recently updated first)
    /// instead of the manual drag order — drives the Sort sessions menu
    /// checkmark.
    var isDateSorted = false
    var onSetSessionDateSorted: (Bool) -> Void = { _ in }

    /// Opens settings on the Presets tab, from the bottom of the
    /// new-session preset menus.
    var onManagePresets: () -> Void = {}
    /// Other local workspaces; empty hides the project "Move to" menu.
    var workspaceMoveTargets: [WorkspaceMoveTarget] = []
    var onMoveToWorkspace: (WorkspaceMoveTarget) -> Void = { _ in }

    /// Worktree children get "Remove worktree" instead of the plain
    /// Remove confirm (ProjectItem.svelte:933).
    private var isWorktreeChild: Bool { node.project.worktreeBranch != nil }

    /// Any inline child folder row — a worktree checkout OR a plain group.
    /// Drives the shared folder-row presentation (chevron mark, session-
    /// level indent); branch-specific bits stay keyed on `isWorktreeChild`.
    private var isChildFolder: Bool { node.project.parentProjectID != nil }

    /// Inline child-folder NAMES share the parent project's normal session
    /// text column. Their disclosure chevron occupies the session mark gutter
    /// to the left, while sessions inside the folder still step in another
    /// level. Plain project headers keep the 7pt base.
    private var rowLeading: CGFloat {
        isChildFolder
            ? 10 + CGFloat(max(0, depth - 1)) * 14
            : 7 + CGFloat(depth) * 14
    }

    @State private var hovering = false

    /// UNPEEL_DEBUG_HOVER_PROJECT=<name|id> forces this row's hover state
    /// and the expanded strip, so snapshots can photograph hover-only UI.
    private static let debugHoverProject =
        ProcessInfo.processInfo.environment["UNPEEL_DEBUG_HOVER_PROJECT"]

    private var debugHover: Bool {
        guard let target = Self.debugHoverProject, !target.isEmpty else { return false }
        return target == node.project.name || target == node.project.id
    }

    private var showsActions: Bool { hovering || debugHover }
    private var folderTint: Color { folderColor?.tint ?? Theme.mutedForeground }

    var body: some View {
        if isConfirmingRemove {
            confirmRemoveRow
        } else {
            normalRow
        }
    }

    /// The row itself becomes the confirmation (same row-swap pattern as
    /// the session remove confirm). The Svelte app removes plain projects
    /// without asking; natively a misclick would tombstone the project, so
    /// the inline confirm stays.
    /// Removing a project also removes every session in its subtree
    /// (UnpeelStore.removeProject), so the confirm names the count.
    /// `node.sessions` already includes archived rows — they are hidden at
    /// display time, not at tree build — so no separate archived term.
    private var removeSessionCount: Int {
        func count(_ n: ProjectNode) -> Int {
            n.sessions.count + n.worktrees.map(count).reduce(0, +)
        }
        return count(node)
    }

    private var confirmRemoveRow: some View {
        HStack(spacing: 7) {
            Text(
                removeSessionCount == 0
                    ? "Remove project?"
                    : removeSessionCount == 1
                        ? "Remove project and 1 session?"
                        : "Remove project and \(removeSessionCount) sessions?"
            )
                .font(Theme.rowLabelFont)
                .foregroundStyle(Theme.foreground)
                .lineLimit(1)

            Spacer(minLength: 4)

            ConfirmPillButton(label: "Cancel", destructive: false, action: onCancelRemove)
            ConfirmPillButton(label: "Remove", destructive: true, action: onConfirmRemove)
        }
        .padding(EdgeInsets(top: 2, leading: rowLeading, bottom: 2, trailing: 5))
        .frame(minHeight: 28)
        .background(
            RoundedRectangle(cornerRadius: 9, style: .continuous)
                .fill(Theme.hoverRow)
        )
        .contentShape(Rectangle())
        .background(RemoveConfirmDismissMonitor(onCancel: onCancelRemove))
    }

    /// Leading mark: Phosphor folder (open/closed by expansion) for plain
    /// projects; worktree children lead with the disclosure chevron in the
    /// parent's folder tint instead (TUI parity). The mark remains stable on
    /// hover even though the whole row is a drag source.
    @ViewBuilder
    private var leadingMark: some View {
        let mark = ZStack {
            if isChildFolder {
                // Inline child folders (worktrees + groups) lead with the
                // disclosure chevron (TUI parity); it rotates open in place
                // of a folder-icon swap.
                ChevronGlyph(glyph: "›", size: 17)
                    .foregroundStyle(Theme.mutedForeground)
                    .opacity(0.6)
                    .rotationEffect(.degrees(isExpanded ? 90 : 0))
                    .animation(.easeInOut(duration: 0.15), value: isExpanded)
            } else {
                ChromeIconView(icon: isExpanded ? .folderOpen : .folderClosed, size: 16)
                    .foregroundStyle(folderTint)
            }
        }
        // The chevron is a narrow glyph — a full 18pt slot reads as a gap
        // between it and the name, so child folder rows tighten the mark.
        .frame(width: isChildFolder ? 11 : 18, height: 18)
        .overlay(alignment: .topTrailing) {
            // .project-state-dot (ProjectItem.svelte:1929-1946):
            // 6px, top/right 1px; attention #eab308 w/ 4px 20% halo.
            if showsAttentionDot {
                AttentionDot(color: Color(hex: 0xEAB308))
                    .padding(EdgeInsets(top: 1, leading: 0, bottom: 0, trailing: -1))
            } else if showsUnreadDot {
                Circle()
                    .fill(Theme.unread)
                    .frame(width: 6, height: 6)
                    .padding(EdgeInsets(top: 1, leading: 0, bottom: 0, trailing: -1))
            }
        }

        mark
    }

    private var normalRow: some View {
        HStack(spacing: 7) {
            leadingMark
                // Keep the disclosure glyph in its current gutter, but move
                // the child-folder name onto the same text column as normal
                // session labels beneath the parent project.
                .padding(.trailing, isChildFolder ? 4 : 0)

            // .project-name: fg at 0.6; while shimmering the CSS sets
            // opacity 1 and sweeps a 80%→100%→80% currentColor gradient
            // across the glyphs (ProjectItem.svelte:2083-2102).
            if showsBusyShimmer {
                ShimmerLabel(
                    text: node.project.name,
                    color: NSColor(Theme.foreground)
                )
            } else {
                Text(node.project.name)
                    .font(Theme.rowLabelFont)
                    .foregroundStyle(
                        isChildFolder ? Theme.foreground : Theme.foreground.opacity(0.6)
                    )
                    .lineLimit(1)
                    .truncationMode(.tail)
            }

            if isPinned {
                SidebarPinnedIndicator()
                    .padding(.leading, -2)
            }

            // A live local site turns the project into a link: the globe
            // right of the name opens it (several sites drop the URL menu).
            if !localSiteURLs.isEmpty {
                ProjectLinkButton(urls: localSiteURLs)
                    .padding(.leading, -3)
            }

            // .project-branch (ProjectItem.svelte:1270-1276, 2195-2215):
            // 12px branch icon + mono branch name at 0.55 opacity; the name
            // is omitted when it equals the project title.
            if let branch = node.project.worktreeBranch {
                HStack(spacing: 3) {
                    ChromeIconView(icon: .branch, size: 12)
                    if branch != node.project.name {
                        Text(branch)
                            .font(.system(size: 10, design: .monospaced))
                            .lineLimit(1)
                            .truncationMode(.tail)
                            .frame(maxWidth: 110, alignment: .leading)
                    }
                }
                .foregroundStyle(Theme.mutedForeground)
                .opacity(0.55)
            }

            Spacer(minLength: 4)

            if showsActions {
                // No disclosure chevron here — the Svelte project row has
                // none either; the whole row toggles open/closed.
                QuickPresetStrip(
                    quickGroups: quickGroups,
                    menuPresets: menuPresets,
                    forceExpanded: debugHover,
                    onLaunch: onLaunchPreset,
                    onManagePresets: onManagePresets,
                    showsManagePresets: showsLocalProjectVerbs,
                    archivedCount: archivedSessionCount,
                    onOpenArchived: onOpenArchived,
                    addableApps: addableApps,
                    onAddApp: onAddApp
                )
            } else if let shortcutHint {
                // Held ⌃ shows the project-switch hint (same 9px/500 @ 0.7
                // treatment as the session rows' ⌘N hint).
                Text("⌃\(shortcutHint)")
                    .font(.system(size: 9, weight: .medium))
                    .foregroundStyle(Theme.mutedForeground)
                    .opacity(0.7)
            }
        }
        .padding(EdgeInsets(top: 2, leading: rowLeading, bottom: 2, trailing: 7))
        .frame(minHeight: 28)
        .background(
            RoundedRectangle(cornerRadius: 9, style: .continuous)
                .fill(showsActions ? Theme.hoverRow : .clear)
        )
        .contentShape(Rectangle())
        .onHover { hovering = $0 }
        .onTapGesture { onToggle() }
        .help("Drag anywhere on the row to reorder")
        // Project context menu (openProjectMenu, ProjectItem.svelte:818-941).
        // Order: New session · [Folder color] · [Sort] · [New worktree…] ·
        // [New group…] · [Move to ▸ Workspace] · [Stop all] · [Archived] ·
        // ── · Reveal in Finder · Open in <Editor> · ── · Remove. Stop all
        // is non-destructive (stopSession per live session — rows stay as
        // exited). Unpeel Sessions MCP access is set per session.
        .contextMenu {
            if (isChildFolder && showsOrganizationVerbs)
                || (node.project.acceptsSessionDrop && canPin) {
                if isChildFolder, showsOrganizationVerbs {
                    Button("Rename") {
                        onRenameWorktree()
                    }
                }
                if node.project.acceptsSessionDrop, canPin {
                    Button(isPinned ? "Unpin" : "Pin") {
                        onSetPinned(!isPinned)
                    }
                }
                Divider()
            }
            Menu("New session") {
                Button("Terminal") { onLaunchPreset(.newTerminal) }
                if !menuPresets.isEmpty {
                    Divider()
                    ForEach(menuPresets) { preset in
                        Button(preset.label) { onLaunchPreset(preset) }
                    }
                }
                if showsLocalProjectVerbs {
                    Divider()
                    Button("Manage Agents & Apps…") { onManagePresets() }
                }
            }
            // Folder color is a MAIN-project verb: groups and worktrees stay
            // neutral so nesting reads by indent, not tint (decided
            // 2026-08-13 after a group picked up a color by accident).
            if node.project.parentProjectID == nil, showsOrganizationVerbs {
                Menu("Folder color") {
                    Button {
                        onSetFolderColor(nil)
                    } label: {
                        FolderColorMenuRow(
                            title: "Default",
                            color: nil,
                            isSelected: folderColor == nil
                        )
                    }
                    Divider()
                    ForEach(ProjectFolderColor.allCases) { color in
                        Button {
                            onSetFolderColor(color)
                        } label: {
                            FolderColorMenuRow(
                                title: color.title,
                                color: color,
                                isSelected: folderColor == color
                            )
                        }
                    }
                }
            }
            // Per-group sort: custom (the manual drag order, the default) or
            // recently updated (last activity, like All recent). Date sort
            // disables drag re-ordering for the group; the stored manual
            // order survives a switch back. Scoped local workspaces flip it
            // through the Host `project.organization.set` carrier.
            if showsOrganizationVerbs {
            Menu("Sort sessions") {
                Picker("Sort sessions", selection: Binding(
                    get: { isDateSorted },
                    set: { onSetSessionDateSorted($0) }
                )) {
                    Text("Custom order").tag(false)
                    Text("Recently updated").tag(true)
                }
                .pickerStyle(.inline)
                .labelsHidden()
            }
            }
            if showsWorktreeCreate {
                Button("New worktree…") {
                    onCreateWorktree()
                }
            }
            if node.project.parentProjectID == nil, showsOrganizationVerbs {
                Button("New group…") {
                    onCreateGroup()
                }
            }
            // File this project (and the sessions under it) into another
            // local workspace on this Mac. Hidden for groups/worktrees and
            // when this Mac only has one workspace.
            if !workspaceMoveTargets.isEmpty {
                Menu("Move to") {
                    Menu("Workspace") {
                        ForEach(workspaceMoveTargets) { target in
                            Button(target.name) { onMoveToWorkspace(target) }
                        }
                    }
                }
            }
            if hasLiveSessions {
                Button("Stop all") {
                    onStopAll()
                }
            }
            if archivedSessionCount > 0 {
                Button("Archived (\(archivedSessionCount))") {
                    onOpenArchived()
                }
            }
            // Local sites this project's sessions are serving (the old
            // titlebar globe dropdown, relocated here).
            if !localSiteURLs.isEmpty {
                Divider()
                ForEach(localSiteURLs, id: \.self) { url in
                    Button {
                        LocalSiteMenu.open(url)
                    } label: {
                        Label {
                            Text(LocalSiteMenu.compactLabel(url))
                        } icon: {
                            if let image = ChromeIconStore.image(for: .globe) {
                                Image(nsImage: image)
                            }
                        }
                    }
                }
            }
            // Reveal / Open in act on the project's REAL path — valid on the
            // same machine, so they show for a scoped local workspace too
            // (its path belongs to this Mac). Open in carries the full
            // target list the titlebar dropdown used to hold.
            if showsFilesystemVerbs {
                Divider()
                Button("Reveal in Finder") {
                    onRevealInFinder()
                }
                Menu("Open in") {
                    let groups = WorkspaceOpenTarget.availableMenuGroups()
                    ForEach(Array(groups.enumerated()), id: \.offset) { index, targets in
                        if index > 0 {
                            Divider()
                        }
                        ForEach(targets) { target in
                            Button {
                                onOpenIn(target)
                            } label: {
                                Label {
                                    Text(target.title)
                                } icon: {
                                    if let icon = WorkspaceAppIconStore.menuImage(for: target) {
                                        Image(nsImage: icon)
                                            .renderingMode(.original)
                                    } else {
                                        Image(systemName: target.fallbackSymbol)
                                    }
                                }
                            }
                        }
                    }
                }
            }
            if isWorktreeChild || isChildFolder {
                // Worktree/group removal writes the scoped home's own
                // records (and runs git against the real path), so it is
                // valid on this Mac; a true remote Host hides it.
                if showsOrganizationVerbs {
                    Divider()
                    if isWorktreeChild {
                        Button("Remove worktree", role: .destructive) {
                            onRemoveWorktree()
                        }
                    } else {
                        // Groups archive their sessions under the parent before
                        // the organizational record is removed.
                        Button("Remove group", role: .destructive) {
                            onRemoveGroup()
                        }
                    }
                }
            } else if showsFilesystemVerbs {
                // Plain project Remove runs local-against the scoped home.
                Divider()
                Button("Remove", role: .destructive) {
                    onRequestRemove()
                }
            }
        }
        .animation(.easeInOut(duration: 0.12), value: hovering)
    }
}

private struct FolderColorMenuRow: View {
    let title: String
    let color: ProjectFolderColor?
    let isSelected: Bool

    var body: some View {
        Label {
            Text(title)
        } icon: {
            Image(nsImage: FolderColorMenuSwatch.image(color: nsColor, selected: isSelected))
                .renderingMode(.original)
        }
    }

    private var nsColor: NSColor {
        color?.nsColor ?? NSColor(hex: 0xB8BCC8)
    }
}

@MainActor
private enum FolderColorMenuSwatch {
    /// Cached per (resolved color, selected): a fresh NSImage on every menu
    /// re-evaluation gives the items a new identity each time the sidebar
    /// re-renders, which makes an open Folder-color submenu blink.
    private static var cache: [String: NSImage] = [:]

    static func image(color: NSColor, selected: Bool) -> NSImage {
        let rgb = color.usingColorSpace(.sRGB) ?? color
        let key = String(
            format: "%.3f-%.3f-%.3f-%.3f-%@",
            rgb.redComponent, rgb.greenComponent, rgb.blueComponent,
            rgb.alphaComponent, selected ? "on" : "off"
        )
        if let cached = cache[key] { return cached }
        let image = draw(color: color, selected: selected)
        cache[key] = image
        return image
    }

    private static func draw(color: NSColor, selected: Bool) -> NSImage {
        let image = NSImage(size: NSSize(width: 18, height: 18))
        image.lockFocus()
        defer { image.unlockFocus() }

        NSGraphicsContext.current?.imageInterpolation = .high

        let shadow = NSShadow()
        shadow.shadowBlurRadius = 2
        shadow.shadowOffset = NSSize(width: 0, height: -0.5)
        shadow.shadowColor = NSColor.black.withAlphaComponent(0.22)
        shadow.set()

        let chip = NSBezierPath(roundedRect: NSRect(x: 3, y: 3, width: 12, height: 12),
                                xRadius: 4, yRadius: 4)
        color.withAlphaComponent(0.94).setFill()
        chip.fill()

        NSShadow().set()
        NSColor.white.withAlphaComponent(0.58).setStroke()
        chip.lineWidth = 1
        chip.stroke()

        if selected {
            let check = NSBezierPath()
            check.move(to: NSPoint(x: 6.1, y: 8.8))
            check.line(to: NSPoint(x: 8.0, y: 6.7))
            check.line(to: NSPoint(x: 12.2, y: 11.4))
            check.lineWidth = 1.8
            check.lineCapStyle = .round
            check.lineJoinStyle = .round
            NSColor.white.setStroke()
            check.stroke()
        }

        image.isTemplate = false
        return image
    }
}

// MARK: - Quick preset strip (ProjectItem.svelte:1292-1357, 1964-2066)

/// The "+" affordance on project-row hover. Collapsed it is a 24px pill
/// showing just the "+"; hovering the pill expands it leftwards
/// (inline-size 0.28s cubic-bezier(0.22,1,0.36,1)) to reveal one icon
/// button per quick preset. The strip is laid out row-reverse in the web
/// app, so visually it reads terminal → … → codex → claude → "+".
/// Clicking an icon launches that preset; "+" is New Session (the Svelte
/// "+" opens the launcher view — the native stand-in is a preset menu).
struct QuickPresetStrip: View {
    /// Starred presets grouped by CLI — one chip per group. A group with a
    /// single starred preset launches it directly; 2+ starred presets of one
    /// CLI render the chip as a dropdown menu of those presets.
    let quickGroups: [QuickPresetGroup]
    let menuPresets: [Preset]
    var forceExpanded = false
    let onLaunch: (Preset) -> Void
    var onManagePresets: () -> Void = {}
    var showsManagePresets = true
    var archivedCount = 0
    var onOpenArchived: (() -> Void)?
    var addableApps: [InstalledAppInfo] = []
    var onAddApp: ((InstalledAppInfo) -> Void)?

    @State private var hovering = false
    @State private var plusHovering = false

    private var expanded: Bool { hovering || forceExpanded }

    /// Expanded width: content is (n+1) 22px chips + n 1px gaps + 3px
    /// horizontal padding each side; add 2px slack so the leftmost chip
    /// keeps its left padding instead of clipping against the pill edge.
    /// (n = CLI chips + the blank-terminal chip.)
    private var expandedWidth: CGFloat { CGFloat(quickGroups.count + 1) * 23 + 30 }

    var body: some View {
        strip
            .onHover { hovering = $0 }
            .animation(
                .timingCurve(0.22, 1, 0.36, 1, duration: 0.28),
                value: expanded
            )
    }

    private var strip: some View {
        HStack(spacing: 1) {
            // row-reverse: the first group (topmost starred CLI) renders
            // rightmost, next to "+"; the blank terminal chip sits leftmost.
            QuickPresetButton(preset: .newTerminal) { onLaunch(.newTerminal) }
            ForEach(quickGroups.reversed()) { group in
                if group.presets.count > 1 {
                    QuickPresetMenuChip(group: group, onLaunch: onLaunch)
                } else {
                    QuickPresetButton(preset: group.leader) { onLaunch(group.leader) }
                }
            }
            newSessionMenu
        }
        .padding(.horizontal, 3)
        .padding(.vertical, 1)
        .frame(width: expanded ? expandedWidth : 28, height: 24, alignment: .trailing)
        .clipped()
    }

    /// The trailing "+" (Svelte: onNewSession → SessionLauncherView, which
    /// lists the blank terminal first, then the presets). The native app
    /// has no launcher screen yet, so this is a menu of the same choices.
    private var newSessionMenu: some View {
        Menu {
            newSessionMenuContent(
                menuPresets: menuPresets,
                onLaunch: onLaunch,
                onManagePresets: onManagePresets,
                showsManagePresets: showsManagePresets,
                archivedCount: archivedCount,
                onOpenArchived: onOpenArchived,
                addableApps: addableApps,
                onAddApp: onAddApp
            )
        } label: {
            // plusIcon (icons.ts:21) at 16px, centered in the same 22×22
            // radius-8 hover chip as QuickPresetButton so hover matches.
            ChromeIconView(icon: .plus, size: 16)
                .foregroundStyle(plusHovering ? Theme.foreground : Theme.mutedForeground)
                .frame(width: 22, height: 22)
        }
        .menuStyle(.borderlessButton)
        .menuIndicator(.hidden)
        .frame(width: 22, height: 22)
        .background(
            RoundedRectangle(cornerRadius: 8, style: .continuous)
                .fill(
                    plusHovering
                        ? Theme.foreground.opacity(0.14)
                        : Theme.mutedForeground.opacity(0.10)
                )
        )
        .background(HoverReporter { plusHovering = $0 })
        .help("New session")
    }
}

/// AppKit tracking-area hover reporter. SwiftUI's `.onHover` does not fire
/// over a `Menu`'s label on macOS (the menu swallows the tracking), so we
/// drop a geometric tracking view behind the menu — `mouseEntered/Exited`
/// fire on cursor crossing regardless of what is drawn on top.
struct HoverReporter: NSViewRepresentable {
    let onChange: (Bool) -> Void

    func makeNSView(context: Context) -> NSView { TrackingView(onChange: onChange) }
    func updateNSView(_ nsView: NSView, context: Context) {
        (nsView as? TrackingView)?.onChange = onChange
    }

    final class TrackingView: NSView {
        var onChange: (Bool) -> Void
        init(onChange: @escaping (Bool) -> Void) {
            self.onChange = onChange
            super.init(frame: .zero)
        }
        @available(*, unavailable) required init?(coder: NSCoder) { nil }

        override func updateTrackingAreas() {
            super.updateTrackingAreas()
            trackingAreas.forEach(removeTrackingArea)
            addTrackingArea(NSTrackingArea(
                rect: bounds,
                options: [.mouseEnteredAndExited, .activeAlways, .inVisibleRect],
                owner: self
            ))
        }
        override func mouseEntered(with event: NSEvent) { onChange(true) }
        override func mouseExited(with event: NSEvent) { onChange(false) }
    }
}

struct PresetMenuButton: View {
    let preset: Preset
    let action: () -> Void

    var body: some View {
        Button(action: action) {
            Label {
                Text(preset.label)
            } icon: {
                ToolIconView(command: preset.command, size: 16)
            }
        }
    }
}

/// Shared "+" new-session dropdown content: blank terminal first, then every
/// available preset, then "Manage presets…". Used by the sidebar project "+"
/// (`QuickPresetStrip.newSessionMenu`), the empty-state placeholder, and the
/// collapsed-sidebar title-bar "+" so all three offer the same presets.
/// `archivedCount`/`onOpenArchived` (project-scoped "+"s only) add an
/// "Archived (N)" entry after the presets that opens the project's archive
/// library in the main pane — same destination as the context-menu item.
@MainActor
@ViewBuilder
func newSessionMenuContent(
    menuPresets: [Preset],
    onLaunch: @escaping (Preset) -> Void,
    onManagePresets: @escaping () -> Void,
    showsManagePresets: Bool = true,
    archivedCount: Int = 0,
    onOpenArchived: (() -> Void)? = nil,
    addableApps: [InstalledAppInfo] = [],
    onAddApp: ((InstalledAppInfo) -> Void)? = nil
) -> some View {
    PresetMenuButton(preset: .newTerminal) {
        onLaunch(.newTerminal)
    }
    if !menuPresets.isEmpty {
        Divider()
        ForEach(menuPresets) { preset in
            PresetMenuButton(preset: preset) {
                onLaunch(preset)
            }
        }
    }
    if let onAddApp, !addableApps.isEmpty {
        Divider()
        Section("Apps you can add") {
            ForEach(addableApps) { app in
                Button {
                    onAddApp(app)
                } label: {
                    Label {
                        Text(app.name)
                    } icon: {
                        Image(systemName: "plus")
                    }
                }
            }
        }
    }
    Divider()
    if archivedCount > 0, let onOpenArchived {
        Button {
            onOpenArchived()
        } label: {
            Label {
                Text("Archived (\(archivedCount))")
            } icon: {
                Image(systemName: "archivebox")
            }
        }
    }
    if showsManagePresets {
        Button {
            onManagePresets()
        } label: {
            Text("Manage Agents & Apps…")
        }
    }
}

/// 22×22 radius-8 icon button: 14px tool icon at 0.72 opacity, muted →
/// foreground + full opacity on hover (ProjectItem.svelte:2048-2066).
private struct QuickPresetButton: View {
    let preset: Preset
    let action: () -> Void

    @State private var hovering = false

    var body: some View {
        Button(action: action) {
            ToolIconView(command: preset.command)
                .opacity(hovering ? 1 : 0.72)
                .foregroundStyle(hovering ? Theme.foreground : Theme.mutedForeground)
                .frame(width: 22, height: 22)
                .background(
                    RoundedRectangle(cornerRadius: 8, style: .continuous)
                        .fill(hovering ? Theme.hoverRow : .clear)
                )
        }
        .buttonStyle(.plain)
        .onHover { hovering = $0 }
        .help("Start \(preset.label)")
    }
}

/// Chip for a CLI with 2+ starred presets: same 22×22 icon chip as
/// `QuickPresetButton`, but clicking opens a menu of that CLI's starred
/// presets (borderless Menu + HoverReporter, like the strip's "+").
private struct QuickPresetMenuChip: View {
    let group: QuickPresetGroup
    let onLaunch: (Preset) -> Void

    @State private var hovering = false

    var body: some View {
        Menu {
            ForEach(group.presets) { preset in
                PresetMenuButton(preset: preset) {
                    onLaunch(preset)
                }
            }
        } label: {
            ToolIconView(command: group.leader.command)
                .opacity(hovering ? 1 : 0.72)
                .foregroundStyle(hovering ? Theme.foreground : Theme.mutedForeground)
                .frame(width: 22, height: 22)
        }
        .menuStyle(.borderlessButton)
        .menuIndicator(.hidden)
        .frame(width: 22, height: 22)
        .background(
            RoundedRectangle(cornerRadius: 8, style: .continuous)
                .fill(hovering ? Theme.hoverRow : .clear)
        )
        .background(HoverReporter { hovering = $0 })
        .help("Start \(group.cli.displayName)…")
    }
}

// MARK: - Session row

/// Exact resume affordance rendered by a sidebar row. Keeping this decision
/// pure makes the active-runtime, returned-shell, and archived states
/// testable without reaching through SwiftUI's private view tree.
enum SessionRowResumePresentation: Equatable {
    case none
    case resumeAgent
    case resumeSession
    case restore
    case restoreAndResume

    var title: String? {
        switch self {
        case .none: return nil
        case .resumeAgent: return "Resume Agent"
        case .resumeSession: return "Resume"
        case .restore: return "Restore from archive"
        case .restoreAndResume: return "Restore & Resume"
        }
    }
}

func sessionRowResumePresentation(
    session: SessionEntry,
    isArchived: Bool,
    canRestart: Bool,
    canResumeAgent: Bool
) -> SessionRowResumePresentation {
    if isArchived {
        return canRestart ? .restoreAndResume : .restore
    }
    guard session.status != .starting else { return .none }
    if session.isLive {
        return canResumeAgent ? .resumeAgent : .none
    }
    return canRestart ? .resumeSession : .none
}

func sessionRowShowsInlineResume(_ presentation: SessionRowResumePresentation) -> Bool {
    presentation == .resumeAgent
        || presentation == .resumeSession
        || presentation == .restoreAndResume
}

/// The representative is only the sidebar anchor for a pane group. Preserve
/// its existing tint when it is working, otherwise let the first working pane
/// drive the collapsed row's spinner so activity never disappears with a
/// hidden member row.
func sessionRowActivitySpinnerCommand(
    session: SessionEntry,
    paneItems: [UnpeelStore.PaneSidebarItem]
) -> String? {
    // Needs-input wins over a sibling spinner so the attention badge stays
    // visible on a collapsed pane group waiting for an MCP approval.
    if session.status == .attention { return nil }
    if session.status == .starting || session.status == .busy {
        return session.presentationCommand
    }
    return paneItems.first(where: {
        $0.status == .starting || $0.status == .busy
    })?.command
}

/// A collapsed multi-pane row cannot identify which conversation the user
/// means. Each pane's own ellipsis menu carries the action instead.
func sessionRowShowsCopyTranscript(
    session: SessionEntry,
    paneItems: [UnpeelStore.PaneSidebarItem]
) -> Bool {
    paneItems.isEmpty && session.supportsTranscriptCopy
}

/// Inline title editor. A dedicated view so `@State` is born with the
/// current label — assigning the draft in `onAppear` left one empty frame
/// (the title blinked) before the field populated.
private struct SessionRowRenameField: View {
    let onCommit: (String) -> Void
    let onCancel: () -> Void

    @State private var draft: String
    @FocusState private var focused: Bool
    @State private var didClaimFocus = false
    @State private var suppressCommit = false

    init(
        initialLabel: String,
        onCommit: @escaping (String) -> Void,
        onCancel: @escaping () -> Void
    ) {
        self.onCommit = onCommit
        self.onCancel = onCancel
        _draft = State(initialValue: initialLabel)
    }

    var body: some View {
        TextField("", text: $draft)
            .textFieldStyle(.plain)
            .font(Theme.sessionLabelFont)
            .foregroundStyle(Theme.foreground)
            .focused($focused)
            .onSubmit(commit) // Enter commits
            .onExitCommand(perform: cancel) // Esc cancels
            .onAppear {
                // Defer one runloop turn so the field is in the window
                // before claiming first responder (select-all happens then).
                DispatchQueue.main.async { focused = true }
            }
            .onChange(of: focused) { isFocused in
                if isFocused {
                    didClaimFocus = true
                    return
                }
                // Ignore the unfocused birth of the field, and a one-turn
                // flicker while first responder is claimed. Click-away still
                // commits, like the Svelte onblur handler
                // (ProjectItem.svelte:1507-1511).
                guard didClaimFocus, !suppressCommit else { return }
                DispatchQueue.main.async {
                    guard !suppressCommit, !focused else { return }
                    commit()
                }
            }
    }

    /// Empty or unchanged input reverts to the original label
    /// (commitEdit, ProjectItem.svelte:958-966).
    private func commit() {
        guard !suppressCommit else { return }
        suppressCommit = true
        onCommit(draft.trimmingCharacters(in: .whitespacesAndNewlines))
    }

    private func cancel() {
        suppressCommit = true
        onCancel()
    }
}

struct SessionRowView: View {
    let session: SessionEntry
    let depth: Int
    /// Leading padding base; depth supplies one standard 14pt nesting step.
    var indentBase: CGFloat = 9
    /// Snapshot rows and detached drag cards supply a fixed selection value.
    /// Live sidebar rows leave that false and hand selection to the tiny
    /// observed background leaf below, so changing tabs does not rebuild the
    /// row's labels, gestures, capability menus, or context-menu hierarchy.
    let isSelected: Bool
    var selectionState: SessionSelectionRowState? = nil
    /// 1-based ⌘ index shown in place of the age while ⌘ is held
    /// (.session-shortcut-hint, ProjectItem.svelte:1536-1540).
    var shortcutHint: Int?
    /// Unread badge: 7px #60a5fa dot in the leading slot (where the busy
    /// spinner sits), lowest precedence after spinner/attention.
    var isUnread = false
    /// Pinning is deliberately context-menu-only. Pinned rows keep a passive
    /// indicator; unpinned rows never gain pin chrome on hover.
    var isPinned = false
    var canPin = true
    /// Inline remove confirmation: the whole row swaps to
    /// "Remove session?" + Remove/Cancel (the Svelte app swaps only the
    /// hover archive button to a "Confirm" pill, ProjectItem.svelte:1562).
    var isConfirmingRemove = false
    /// Inline archive confirmation, shown only for actively-working sessions
    /// (archiving stops the agent mid-turn); settled rows archive directly.
    var isConfirmingArchive = false
    /// Kill/cleanup in flight: row disabled, meta shows "removing".
    var isRemoving = false
    /// Restart in flight: row disabled, meta shows "restarting"
    /// (ProjectItem.svelte:1534-1535).
    var isRestarting = false
    /// In-place provider resume; the terminal remains mounted.
    var isResumingAgent = false
    /// Archive in flight (live host still stopping): row muted + disabled,
    /// leading slot shows a muted spinner, meta shows "archiving". The row
    /// disappears into the archive once the stop completes.
    var isArchiving = false
    /// Already archived (a recent archive still showing in the stopped
    /// group): the archive affordances swap to Restore.
    var isArchived = false
    /// Inline rename editor (double-click on the label or context-menu
    /// Rename — `editingSessionId` in ProjectItem.svelte:146): the label
    /// swaps to a TextField pre-filled with the current title. Enter
    /// commits, Esc cancels, click-away (focus loss) commits — matching the
    /// Svelte contenteditable's keydown/blur handlers
    /// (ProjectItem.svelte:958-986, 1507-1511). Empty/unchanged input
    /// reverts to the original label.
    var isEditing = false
    /// Whether this session is opted into a "finished" phone push.
    var notifyWhenDone = false
    /// Whether Resume continues this stopped Session's provider conversation.
    /// Gates the Resume item
    /// (ProviderCapabilities.canRestart).
    var canRestart = true
    /// Whether an ended managed launch can resume inside this same terminal.
    var canResumeAgent = false
    /// Whether Stop and archive / Archive preserves a resumable launch.
    var canArchive = true
    /// Whether this session's provider reports turn completion through hooks,
    /// making the "Notify when done" push reliable. Gates that toggle.
    var canNotifyWhenDone = true
    /// Whether "Clear attention" applies — the Controller-local activity
    /// engine's escape hatch, hidden for remote Hosts.
    var canClearAttention = true
    /// This row represents an open pane group. Its participating runtime
    /// marks replace the single CLI logo as a bordered, overlapping stack.
    var paneItems: [UnpeelStore.PaneSidebarItem] = []
    /// Recently-updated groups show the lifecycle event age that ranked the
    /// row; custom groups leave this nil and show no date.
    var ageTimestampMs: Int64? = nil
    let onSelect: () -> Void
    /// Focus one member of the pane group represented by this collapsed row.
    var onFocusPane: (String) -> Void = { _ in }
    /// Fired only after a deliberate pointer dwell — switch intent. Keeping
    /// this well behind the visual hover prevents a fast sweep through a long
    /// list from mounting terminal panes while the pointer is still moving.
    var onHoverIntent: () -> Void = {}
    /// Opt this session in/out of the "notify when done" push.
    var onSetNotifyWhenDone: (Bool) -> Void = { _ in }
    var onResume: () -> Void = {}
    /// Force-clear a stuck/false attention badge (offered only while the
    /// row shows one).
    var onClearAttention: () -> Void = {}
    var onSetPinned: (Bool) -> Void = { _ in }
    var onRequestRemove: () -> Void = {}
    var onConfirmRemove: () -> Void = {}
    var onCancelRemove: () -> Void = {}
    /// Archive this session (stop it if running, keep everything on disk,
    /// move the row into the bottom archive section/library). Busy sessions
    /// confirm inline.
    var onArchive: () -> Void = {}
    /// Restore a recent archived row, resuming when its launch supports it.
    var onUnarchive: () -> Void = {}
    /// Confirmed archive from the inline row — skips the busy re-check that
    /// `onArchive` routes through (it would just re-arm the confirm).
    var onConfirmArchive: () -> Void = {}
    var onCancelArchive: () -> Void = {}
    /// Copy this session's conversation transcript as Markdown (rendered by the
    /// host using the shared Settings ▸ Transcripts content toggles). The Int
    /// is the flyout's range pick: entry count, or 0 for the whole conversation.
    var onCopyTranscript: (String, Int) -> Void = { _, _ in }
    var onBeginEdit: () -> Void = {}
    var onCommitRename: (String) -> Void = { _ in }
    var onEndEdit: () -> Void = {}
    /// "Move to" destinations (plain groups, or back to the root project);
    /// empty hides the menu. Worktrees require restart/resume instead.
    var moveTargets: [SessionMoveTarget] = []
    var onMoveTo: (String) -> Void = { _ in }
    /// Files the session into its project's right-panel "Sidebar" group;
    /// nil hides the item (remote scope, or already there).
    var onMoveToProjectSidebar: (() -> Void)? = nil
    /// Un-files a sidebar-group member back to its own project; nil hides it.
    var onMoveToMainArea: (() -> Void)? = nil

    @State private var hovering = false
    @State private var hoverIntentTask: Task<Void, Never>?

    /// UNPEEL_DEBUG_HOVER_SESSION=<session-id> forces this row's hover
    /// state so snapshots can photograph its hover actions.
    private static let debugHoverSession =
        ProcessInfo.processInfo.environment["UNPEEL_DEBUG_HOVER_SESSION"]
    private static let hoverIntentDelay: UInt64 = 450_000_000

    private var isHovering: Bool {
        hovering || Self.debugHoverSession == session.id
    }

    var body: some View {
        if isConfirmingRemove {
            confirmRemoveRow
        } else if isConfirmingArchive {
            confirmArchiveRow
        } else {
            normalRow
        }
    }

    private var normalRow: some View {
        HStack(spacing: 7) {
            leadingSlot

            if isEditing {
                SessionRowRenameField(
                    initialLabel: session.label,
                    onCommit: { newValue in
                        if !newValue.isEmpty && newValue != session.label {
                            onCommitRename(newValue)
                        }
                        onEndEdit()
                    },
                    onCancel: onEndEdit
                )
            } else {
                HStack(spacing: 5) {
                    HoverMarqueeSessionTitle(
                        session.label,
                        isHovering: isHovering
                    )
                        // Double-click the label → inline rename
                        // (ondblclick → startEditing, ProjectItem.svelte:1503).
                        // simultaneousGesture so the row's single-tap select is
                        // never blocked by the pending double-tap.
                        .simultaneousGesture(
                            TapGesture(count: 2).onEnded { onBeginEdit() }
                        )

                    if isPinned {
                        SidebarPinnedIndicator()
                    }
                }
            }

            Spacer(minLength: 4)

            // Trailing cluster: restart/archive and the stable meta slot.
            HStack(spacing: 4) {
                // The cluster is trailing-aligned, so hover actions grow
                // leftward without moving the age or runtime mark.
                if isHovering, showsInlineResume, !isRemoving,
                   !isRestarting, !isResumingAgent, !isArchiving {
                    RestartActionButton(help: inlineResumeHelp, action: onResume)
                        .padding(.trailing, -2)
                }

                if isRemoving || isRestarting || isResumingAgent || isArchiving {
                    Text(
                        isRemoving
                            ? "removing"
                            : isArchiving
                                ? "archiving"
                                : isResumingAgent
                                    ? "resuming agent"
                                    : (session.isLive ? "reloading" : "resuming")
                    )
                    .font(.system(size: 9))
                    .opacity(0.7)
                } else {
                    // Fixed-width meta slot so the hover swap (age → archive)
                    // never changes the cluster width; only the trailing
                    // content cross-fades in place.
                    ZStack(alignment: .trailing) {
                        if isHovering, !isArchived {
                            // Hover swap: the age hides and the action
                            // affordances appear. Overflowing the slot via the
                            // negative padding nudges the 13px glyphs to the
                            // row's trailing edge without reflowing the row.
                            // Non-resumable commands can't meaningfully be
                            // archived (nothing to resume later), so their
                            // clear-it-out affordance is Remove.
                            if canArchive {
                                ArchiveActionButton(
                                    help: session.isLive ? "Stop and archive" : "Archive",
                                    action: onArchive
                                )
                                .padding(.trailing, -4)
                            } else {
                                // The row-level X is deliberately immediate:
                                // this launch has nothing resumable to save,
                                // so kill/delete it instead of routing through
                                // Archive. Context-menu Remove still confirms.
                                RemoveActionButton(action: onConfirmRemove)
                                    .padding(.trailing, -4)
                            }
                        } else if isHovering, isArchived {
                            // Every archived row keeps an immediate X: the
                            // conversation is already filed away, so deleting
                            // it is a one-click cleanup (Resume, when the
                            // archive is resumable, sits to the left).
                            RemoveActionButton(action: onConfirmRemove)
                                .padding(.trailing, -4)
                        } else if let shortcutHint {
                            // Held ⌘ swaps the age for the ⌘N hint
                            // (ProjectItem.svelte:1536-1540, 9px/500 @ 0.7).
                            Text("⌘\(shortcutHint)")
                                .font(.system(size: 9, weight: .medium))
                                .opacity(0.7)
                        } else if let ageTimestampMs {
                            Text(session.ageString(since: ageTimestampMs))
                                .font(.system(size: 9))
                                .opacity(0.7)
                        }
                    }
                    .frame(width: 24, alignment: .trailing)
                }

                SessionCommandIconPresentation(
                    command: session.presentationCommand,
                    items: paneItems,
                    onFocus: onFocusPane
                )
                .padding(.leading, 3)
            }
        }
        // Title ↔ editor is a discrete swap. Inherited list animations
        // interpolated it as a fade and made the double-click rename blink.
        .animation(nil, value: isEditing)
        .foregroundStyle(
            session.isLive && !isArchiving ? Theme.foreground : Theme.mutedForeground
        )
        // Exited rows read as clearly stopped: a hard dim, not the barely
        // visible 0.82 wash they used to get. Hover lifts the dim so the
        // Restart/Archive affordances (and the label) stay readable.
        .opacity(
            (isRemoving || isRestarting || isResumingAgent || isArchiving)
                ? 0.5
                : (session.isLive ? 1 : (isHovering ? 0.9 : 0.55))
        )
        .padding(EdgeInsets(top: 2, leading: indentBase + CGFloat(depth) * 14, bottom: 2, trailing: 9))
        .frame(minHeight: Theme.sessionRowHeight)
        .background {
            SessionRowSelectionBackground(
                state: selectionState,
                fallbackIsSelected: isSelected,
                isHovering: isHovering
            )
        }
        .contentShape(Rectangle())
        .onHover { inside in
            guard hovering != inside else { return }
            hovering = inside
            hoverIntentTask?.cancel()
            hoverIntentTask = nil
            if inside, session.isAttachable,
               !(selectionState?.isSelected ?? isSelected) {
                hoverIntentTask = Task { @MainActor in
                    try? await Task.sleep(nanoseconds: Self.hoverIntentDelay)
                    guard !Task.isCancelled else { return }
                    onHoverIntent()
                }
            }
        }
        .onDisappear {
            hoverIntentTask?.cancel()
            hoverIntentTask = nil
        }
        .onTapGesture { if !isEditing { onSelect() } }
        .contextMenu {
            regularContextMenuItems
        }
        .allowsHitTesting(!isRemoving && !isRestarting && !isResumingAgent && !isArchiving)
    }

    @ViewBuilder
    private var regularContextMenuItems: some View {
        // Same first item as the Svelte session context menu
        // (session-menu-rename, ProjectItem.svelte:1038-1043).
        Button("Rename") {
            onBeginEdit()
        }
        if canPin {
            Button(isPinned ? "Unpin" : "Pin") {
                onSetPinned(!isPinned)
            }
        }
        // Files the session into the project's pinned "Sidebar" group — the
        // desktop shows those members as a right-side terminal stack, and the
        // TUI/phone find them in that top group like any other group. The
        // counterpart un-files a sidebar member back to its own project.
        if let onMoveToProjectSidebar {
            Button("Pin to global project sidebar") {
                onMoveToProjectSidebar()
            }
        }
        if let onMoveToMainArea {
            Button("Unpin from global project sidebar") {
                onMoveToMainArea()
            }
        }
        // File the session under a plain group (or back to the root project)
        // using a shared project-override marker, so the TUI and phone see
        // the same placement. Worktrees require restart/resume.
        if !moveTargets.isEmpty {
            Menu("Move to") {
                ForEach(moveTargets) { target in
                    Button(target.name) { onMoveTo(target.id) }
                }
            }
        }
        // Escape hatch for a stuck or false attention badge (a missed
        // hook, or menu detection tripping on look-alike screen text) —
        // otherwise nothing short of answering/restarting clears it.
        if session.status == .attention, canClearAttention {
            Button("Clear attention") {
                onClearAttention()
            }
        }
        // Verb items are capability-gated per CLI (ProviderCapabilities —
        // the same answers the phone's session sheet gets): no Resume for
        // commands whose conversation a relaunch would silently lose and no
        // notify toggle without hook Stop events.
        // (There is no separate "Stop" verb: "Stop and archive" below is
        // the stop — it kills the hosted terminal and files the row into the
        // bottom archive section, from where Resume continues the conversation.)
        if session.status != .starting {
            // An ended managed launch can resume from the still-live shell in
            // the existing terminal. A stopped Session uses the legacy
            // replacement operation and presents that honestly as Resume.
            if resumePresentation == .resumeAgent {
                Button("Resume Agent") {
                    onResume()
                }
            } else if resumePresentation == .resumeSession {
                Button("Resume") {
                    onResume()
                }
            }
        }
        // "Notify when done" — push a phone notification when this session
        // next finishes a turn (paired iPhone with notifications on).
        if canNotifyWhenDone {
            Toggle("Notify when done", isOn: Binding(
                get: { notifyWhenDone },
                set: { onSetNotifyWhenDone($0) }
            ))
        }
        Divider()
        if sessionRowShowsCopyTranscript(session: session, paneItems: paneItems) {
            copyTranscriptMenu(sessionID: session.id)
        }
        // Every Unpeel Session has an id. It is useful beyond agent/MCP
        // features, so keep it as a direct session-level action.
        Button("Copy session ID") {
            copyToPasteboard("Unpeel Session ID: \(session.id)")
        }
        Divider()
        // Archive is the non-destructive "file it away" — and for live
        // sessions it is also the stop verb: kill the hosted terminal, keep
        // the whole session on disk, move the row into the archive section.
        // Working sessions get an inline confirm first. Already-archived
        // resumable rows offer the combined Restore & Resume action; legacy
        // non-resumable archives can still be plainly restored.
        if isArchived {
            Button(resumePresentation.title ?? "Restore from archive") {
                onUnarchive()
            }
        } else if canArchive {
            // Only resumable commands offer Archive; for the rest, Remove
            // below is the sole clear-it-out verb.
            Button(session.isLive ? "Stop and archive" : "Archive") {
                onArchive()
            }
        }
        // Remove stays the explicit destructive verb: it deletes Unpeel's
        // session directory (terminal output and artifacts). Provider
        // transcripts stay in the agent's own storage.
        Button(session.isLive ? "Remove session" : "Remove from list", role: .destructive) {
            onRequestRemove()
        }
    }

    @ViewBuilder
    private func copyTranscriptMenu(sessionID: String) -> some View {
        // Content toggles still come from Settings ▸ Transcripts; 0 means
        // the complete provider conversation.
        Menu("Copy transcript") {
            Button("Last 20 entries") {
                onCopyTranscript(sessionID, 20)
            }
            Button("Last 50 entries") {
                onCopyTranscript(sessionID, 50)
            }
            Button("Whole conversation") {
                onCopyTranscript(sessionID, 0)
            }
        }
        .help("Copy the conversation as Markdown, using your Settings ▸ Transcripts content options")
    }

    private func copyToPasteboard(_ value: String) {
        NSPasteboard.general.clearContents()
        NSPasteboard.general.setString(value, forType: .string)
    }

    /// The row itself becomes the confirmation: question + destructive
    /// Remove + Cancel. Esc or clicking anywhere else cancels.
    private var confirmRemoveRow: some View {
        HStack(spacing: 7) {
            Text(session.isLive ? "Remove session?" : "Remove from list?")
                .font(Theme.rowLabelFont)
                .foregroundStyle(Theme.foreground)
                .lineLimit(1)

            Spacer(minLength: 4)

            ConfirmPillButton(label: "Cancel", destructive: false, action: onCancelRemove)
            ConfirmPillButton(label: "Remove", destructive: true, action: onConfirmRemove)
        }
        .padding(EdgeInsets(top: 2, leading: indentBase + CGFloat(depth) * 14, bottom: 2, trailing: 5))
        .frame(minHeight: Theme.sessionRowHeight)
        .background(
            RoundedRectangle(cornerRadius: 9, style: .continuous)
                .fill(Theme.hoverRow)
        )
        .contentShape(Rectangle())
        .background(RemoveConfirmDismissMonitor(onCancel: onCancelRemove))
    }

    /// Archive confirmation — only reached for actively-working sessions
    /// (archiving stops the agent mid-turn; settled rows archive without
    /// asking). Same inline pattern as the remove confirm.
    private var confirmArchiveRow: some View {
        HStack(spacing: 7) {
            Text("Stop and archive session?")
                .font(Theme.rowLabelFont)
                .foregroundStyle(Theme.foreground)
                .lineLimit(1)

            Spacer(minLength: 4)

            ConfirmPillButton(label: "Cancel", destructive: false, action: onCancelArchive)
            ConfirmPillButton(label: "Archive", destructive: true, action: onConfirmArchive)
        }
        .padding(EdgeInsets(top: 2, leading: indentBase + CGFloat(depth) * 14, bottom: 2, trailing: 5))
        .frame(minHeight: Theme.sessionRowHeight)
        .background(
            RoundedRectangle(cornerRadius: 9, style: .continuous)
                .fill(Theme.hoverRow)
        )
        .contentShape(Rectangle())
        .background(RemoveConfirmDismissMonitor(onCancel: onCancelArchive))
    }

    /// Fixed leading icon slot (ProjectItem.svelte .session-leading,
    /// :2355-2364): EVERY row reserves a constant 16×16 slot so labels align
    /// whether or not a status is showing. Status occupants stack and
    /// cross-fade in place so the label never shifts.
    @ViewBuilder
    private var leadingSlot: some View {
        let activitySpinnerCommand = sessionRowActivitySpinnerCommand(
            session: session,
            paneItems: paneItems
        )
        let spinnerCommand = isRestarting || isResumingAgent
            ? session.presentationCommand
            : activitySpinnerCommand
        let slot = ZStack {
            // Any busy pane (or a restarting representative) → spinner;
            // otherwise attention shows the 6px #f59e0b dot. Dragging never
            // replaces status with a grip, so the leading signal stays stable
            // while mousing over the row.
            if isArchiving {
                // Stopping on the way into the archive: a muted spinner, not
                // the tool-tinted busy one — the session is winding down.
                BrailleSpinner(color: Theme.mutedForeground)
            } else if let spinnerCommand {
                BrailleSpinner(color: Theme.toolSpinnerColor(forCommand: spinnerCommand))
            } else if session.status == .attention {
                AttentionDot(color: Theme.attention)
            } else if isUnread {
                // Done-and-not-looked-at: the blue dot takes the spinner's
                // slot, so "working" hands off to "done" in place (and the
                // iOS sidebar's leading column matches).
                Circle()
                    .fill(Theme.unread)
                    .frame(width: 7, height: 7)
            }
            // Exited rows show no marker: the hard dim is signal enough.
        }
        .frame(width: 16, height: 16)
        .animation(.easeInOut(duration: 0.12), value: session.status)
        .animation(.easeInOut(duration: 0.12), value: activitySpinnerCommand)
        .animation(.easeInOut(duration: 0.12), value: isUnread)

        slot
    }

    /// Every actually resumable state gets an inline hover action: a managed
    /// agent that returned to its live shell, a stopped Session, or a
    /// resumable archive. Archive/Remove eligibility is independent.
    private var showsInlineResume: Bool {
        sessionRowShowsInlineResume(resumePresentation)
    }

    private var inlineResumeHelp: String {
        switch resumePresentation {
        case .resumeAgent: return "Resume agent in this terminal"
        case .restoreAndResume: return "Restore and resume session"
        default: return "Resume session (continues the conversation)"
        }
    }

    private var resumePresentation: SessionRowResumePresentation {
        sessionRowResumePresentation(
            session: session,
            isArchived: isArchived,
            canRestart: canRestart,
            canResumeAgent: canResumeAgent
        )
    }

}

/// Status only: Pin/Unpin itself remains in the context menu. Keeping
/// this non-interactive avoids reintroducing an unpinned hover affordance.
private struct SidebarPinnedIndicator: View {
    var body: some View {
        ChromeIconView(icon: .pin, size: 12)
            .foregroundStyle(Theme.foreground.opacity(0.88))
            .frame(width: 14, height: 18)
            .fixedSize()
            .accessibilityLabel("Pinned")
            .allowsHitTesting(false)
    }
}

/// Attention indicator (DESIGN.md §5 / .session-status.attention,
/// ProjectItem.svelte:2492-2512): static 6px dot with a 4px halo at 20%
/// of the dot color. No animation.
struct AttentionDot: View {
    let color: Color

    var body: some View {
        Circle()
            .fill(color)
            .frame(width: 6, height: 6)
            .background(
                Circle()
                    .fill(color.opacity(0.20))
                    .frame(width: 14, height: 14)
            )
    }
}

// MARK: - Remove session (inline confirm) building blocks

/// 22×22 radius-6 archive button shown in the meta slot on row hover
/// (.session-archive-action, ProjectItem.svelte:2700-2741): 13px archive
/// glyph, muted → foreground + fg-10% bg on hover.
private struct ArchiveActionButton: View {
    var help = "Archive session"
    let action: () -> Void

    @State private var hovering = false

    /// archiveIcon (icons.ts:32), same SVG→template pipeline as ChromeIcons.
    @MainActor private static let archiveImage: NSImage? = {
        let svg = ##"<svg xmlns="http://www.w3.org/2000/svg" width="16" height="16" fill="#FFFFFF" viewBox="0 0 256 256"><path d="M224,50H32A14,14,0,0,0,18,64V88a14,14,0,0,0,14,14h2v90a14,14,0,0,0,14,14H208a14,14,0,0,0,14-14V102h2a14,14,0,0,0,14-14V64A14,14,0,0,0,224,50ZM210,192a2,2,0,0,1-2,2H48a2,2,0,0,1-2-2V102H210ZM226,88a2,2,0,0,1-2,2H32a2,2,0,0,1-2-2V64a2,2,0,0,1,2-2H224a2,2,0,0,1,2,2ZM98,136a6,6,0,0,1,6-6h48a6,6,0,0,1,0,12H104A6,6,0,0,1,98,136Z"></path></svg>"##
        let image = NSImage(data: Data(svg.utf8))
        image?.isTemplate = true
        return image
    }()

    var body: some View {
        Button {
            action()
        } label: {
            Group {
                if let image = Self.archiveImage {
                    Image(nsImage: image)
                        .resizable()
                        .interpolation(.high)
                        .aspectRatio(contentMode: .fit)
                        .frame(width: 13, height: 13)
                } else {
                    Image(systemName: "archivebox")
                        .font(.system(size: 11, weight: .medium))
                }
            }
            .foregroundStyle(hovering ? Theme.foreground : Theme.mutedForeground)
            .frame(width: 22, height: 22)
            .background(
                RoundedRectangle(cornerRadius: 6, style: .continuous)
                    .fill(hovering ? Theme.hoverRow : .clear)
            )
        }
        .buttonStyle(.plain)
        .onHover { hovering = $0 }
        .help(help)
    }
}

/// The ArchiveActionButton's destructive sibling, shown on rows whose
/// command can't resume (nothing to archive FOR): same 22×22 hover
/// treatment and X glyph. The row-level X is the explicit immediate
/// kill/delete affordance; the context-menu Remove verb still confirms.
private struct RemoveActionButton: View {
    let action: () -> Void

    @State private var hovering = false

    var body: some View {
        Button {
            action()
        } label: {
            Image(systemName: "xmark")
                .font(.system(size: 11, weight: .medium))
                .foregroundStyle(hovering ? Theme.foreground : Theme.mutedForeground)
                .frame(width: 22, height: 22)
                .background(
                    RoundedRectangle(cornerRadius: 6, style: .continuous)
                        .fill(hovering ? Theme.hoverRow : .clear)
                )
        }
        .buttonStyle(.plain)
        .onHover { hovering = $0 }
        .help("Remove session")
    }
}

/// 22×22 radius-6 restart button shown when an exited row is hovered: 13px
/// Phosphor "arrow-clockwise" glyph, muted →
/// foreground + fg-10% bg on hover (same treatment as ArchiveActionButton).
private struct RestartActionButton: View {
    var help = "Resume session (continues the conversation)"
    let action: () -> Void

    @State private var hovering = false

    @MainActor private static let restartImage: NSImage? = {
        let svg = ##"<svg xmlns="http://www.w3.org/2000/svg" width="16" height="16" fill="#FFFFFF" viewBox="0 0 256 256"><path d="M240,56v48a8,8,0,0,1-8,8H184a8,8,0,0,1,0-16H211.4L184.81,71.64l-.25-.24a80,80,0,1,0-1.67,114.78,8,8,0,0,1,11,11.63A95.44,95.44,0,0,1,128,224h-1.32A96,96,0,1,1,195.75,60L224,86V56a8,8,0,0,1,16,0Z"></path></svg>"##
        let image = NSImage(data: Data(svg.utf8))
        image?.isTemplate = true
        return image
    }()

    var body: some View {
        Button {
            action()
        } label: {
            Group {
                if let image = Self.restartImage {
                    Image(nsImage: image)
                        .resizable()
                        .interpolation(.high)
                        .aspectRatio(contentMode: .fit)
                        .frame(width: 13, height: 13)
                } else {
                    Image(systemName: "arrow.clockwise")
                        .font(.system(size: 11, weight: .medium))
                }
            }
            .foregroundStyle(hovering ? Theme.foreground : Theme.mutedForeground)
            .frame(width: 22, height: 22)
            .background(
                RoundedRectangle(cornerRadius: 6, style: .continuous)
                    .fill(hovering ? Theme.hoverRow : .clear)
            )
        }
        .buttonStyle(.plain)
        .onHover { hovering = $0 }
        .help(help)
    }
}

/// Non-interactive runtime mark shown right of the date. Fixed-size so the
/// hover swap in the adjacent meta slot never reflows it. The asset comes
/// from the runtime catalog
/// (`display.kind` + optional `icon_asset`), not a client provider table.
private struct SessionCommandIcon: View {
    let command: String

    var body: some View {
        ToolIconView(command: command, size: 12)
            .opacity(0.82)
            .frame(width: 14, height: 14)
            .help(command.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty
                ? "Terminal"
                : command)
    }
}

/// Keeps the standalone mark and merged stack in one persistent layout slot,
/// so merging and separating panes can animate without a hard width jump.
private struct SessionCommandIconPresentation: View {
    let command: String
    let items: [UnpeelStore.PaneSidebarItem]
    let onFocus: (String) -> Void

    var body: some View {
        Group {
            if items.isEmpty {
                SessionCommandIcon(command: command)
                    .transition(.scale(scale: 0.82).combined(with: .opacity))
            } else {
                SessionCommandIconStack(items: items, onFocus: onFocus)
                    .transition(.scale(scale: 0.82).combined(with: .opacity))
            }
        }
        .animation(
            .spring(response: 0.36, dampingFraction: 0.76),
            value: items.map(\.id)
        )
    }
}

/// A pane group is still one sidebar row, but it represents several independent
/// sessions. Keep their own runtime identity instead of replacing it with a
/// generic columns glyph: small opaque tiles overlap like an avatar stack,
/// and the hairline around every tile keeps same-color marks distinct.
private struct SessionCommandIconStack: View {
    let items: [UnpeelStore.PaneSidebarItem]
    let onFocus: (String) -> Void

    private var visibleItems: [UnpeelStore.PaneSidebarItem] {
        Array(items.prefix(4))
    }

    var body: some View {
        HStack(spacing: -8) {
            ForEach(Array(visibleItems.enumerated()), id: \.element.id) { index, item in
                Button {
                    onFocus(item.sessionID)
                } label: {
                    SessionCommandIconStackTile(
                        command: item.command,
                        index: index,
                        stackCount: visibleItems.count
                    )
                }
                .buttonStyle(.plain)
                .help(item.isRepresentative ? "Focus representative pane" : "Focus pane")
                .transition(
                    .asymmetric(
                        insertion: .identity,
                        removal: .scale(scale: 0.72).combined(with: .opacity)
                    )
                )
            }
        }
        // Stable session identities let SwiftUI spring each existing logo to
        // its new slot when panes reorder. The same spring handles insertion
        // into an existing merged stack.
        .animation(
            .spring(response: 0.36, dampingFraction: 0.76),
            value: visibleItems.map(\.id)
        )
        .opacity(0.9)
        .frame(height: 20)
        .accessibilityElement(children: .contain)
        .accessibilityLabel("Multi-pane view with \(items.count) sessions")
    }
}

/// One member of the runtime stack. Each new tile owns its reveal state, so
/// adding a third/fourth pane springs only that new logo in; existing marks
/// do not replay their entrance. Alternating sub-degree rotations keep the
/// settled stack visibly layered without turning it into a fan.
private struct SessionCommandIconStackTile: View {
    let command: String
    let index: Int
    let stackCount: Int

    @State private var revealed = false

    private var settledRotation: Double {
        [-0.8, 0.9, -0.6, 0.7][min(index, 3)]
    }

    var body: some View {
        ToolIconView(command: command, size: 12)
            .frame(width: 18, height: 18)
            .background(
                // The sidebar's own surface recipe (material + tint wash),
                // so the tiles read as raised sidebar glass rather than
                // opaque terminal-colored squares punching through it.
                RoundedRectangle(cornerRadius: 5, style: .continuous)
                    .fill(.ultraThinMaterial)
                    .overlay(
                        RoundedRectangle(cornerRadius: 5, style: .continuous)
                            .fill(Theme.sidebarTint)
                    )
            )
            .overlay(
                RoundedRectangle(cornerRadius: 5, style: .continuous)
                    .stroke(Theme.foreground.opacity(0.24), lineWidth: 0.8)
            )
            .scaleEffect(revealed ? 1 : 0.68)
            .rotationEffect(.degrees(revealed ? settledRotation : settledRotation * 3))
            .offset(x: revealed ? 0 : -4)
            .opacity(revealed ? 1 : 0)
            // The first tile is the visible/frontmost member; each later tile
            // peeks out to its trailing side.
            .zIndex(Double(stackCount - index))
            .task {
                guard !revealed else { return }
                try? await Task.sleep(
                    nanoseconds: UInt64(index) * 45_000_000
                )
                guard !Task.isCancelled else { return }
                withAnimation(.spring(response: 0.36, dampingFraction: 0.76)) {
                    revealed = true
                }
            }
    }
}

// MARK: - Liquid Glass selected row (macOS 26+)

extension SessionRowView {
    /// Resolved once: whether the OS has Liquid Glass; pre-26 keeps the
    /// flat Theme.activeRow fill.
    static let liquidGlassAvailable: Bool = {
        if #available(macOS 26.0, *) { return true }
        return false
    }()
}

/// Selection updates terminate here. Live rows observe their per-id token in
/// this background-only leaf; snapshot/drag rows use the fixed fallback.
private struct SessionRowSelectionBackground: View {
    let state: SessionSelectionRowState?
    let fallbackIsSelected: Bool
    let isHovering: Bool

    @ViewBuilder
    var body: some View {
        if let state {
            ObservedSessionRowSelectionBackground(
                state: state,
                isHovering: isHovering
            )
        } else {
            SessionRowSelectionPaint(
                isSelected: fallbackIsSelected,
                isHovering: isHovering
            )
        }
    }
}

private struct ObservedSessionRowSelectionBackground: View {
    @ObservedObject var state: SessionSelectionRowState
    let isHovering: Bool

    var body: some View {
        SessionRowSelectionPaint(
            isSelected: state.isSelected,
            isHovering: isHovering
        )
    }
}

private struct SessionRowSelectionPaint: View {
    let isSelected: Bool
    let isHovering: Bool

    var body: some View {
        RoundedRectangle(cornerRadius: 9, style: .continuous)
            .fill(
                isSelected
                    ? (SessionRowView.liquidGlassAvailable ? .clear : Theme.activeRowGlassTint)
                    : (isHovering ? Theme.hoverRow : .clear)
            )
            // Selected row: real Liquid Glass with a light wash of the
            // workspace tint, plus a rim for low-contrast backdrops.
            .selectedRowGlass(isSelected)
            // Selection is navigation, not an animated state transition.
            // In particular, prevent Liquid Glass insertion from adding a
            // compositor-side ease after the content pane has already moved.
            .transaction { $0.disablesAnimations = true }
    }
}

/// Bumps whenever any window becomes or resigns key. The Liquid Glass
/// effect samples the window's focus state when its backing view is
/// created and does not reliably re-evaluate on key changes — a chip born
/// in an unfocused window keeps the dim variant. Keying the glass view's
/// identity off this generation rebuilds it (one view, the selected row
/// only) so it always renders the current focus style.
@MainActor
private final class WindowKeyState: ObservableObject {
    static let shared = WindowKeyState()

    @Published private(set) var generation = 0
    private var observers: [NSObjectProtocol] = []
    private var settleWorkItem: DispatchWorkItem?

    private init() {
        let center = NotificationCenter.default
        for name in [
            NSWindow.didBecomeKeyNotification,
            NSWindow.didResignKeyNotification,
            NSApplication.didBecomeActiveNotification,
            NSApplication.didResignActiveNotification,
        ] {
            observers.append(center.addObserver(
                forName: name, object: nil, queue: .main
            ) { [weak self] _ in
                MainActor.assumeIsolated { self?.noteFocusTransition() }
            })
        }
    }

    /// Immediate rebuild plus one settle-delayed rebuild. A chip created
    /// DURING an activation transition (click-through onto a row activates
    /// the app AND moves the selection in the same beat) can be born after
    /// the key notification already fired but before the window's key state
    /// settled — a single synchronous bump misses it and the dim variant
    /// sticks until the next manual unfocus/refocus. The trailing bump
    /// rebuilds the one selected-row glass view once more after the
    /// transition has settled.
    private func noteFocusTransition() {
        generation += 1
        settleWorkItem?.cancel()
        let work = DispatchWorkItem { [weak self] in
            MainActor.assumeIsolated { self?.generation += 1 }
        }
        settleWorkItem = work
        DispatchQueue.main.asyncAfter(deadline: .now() + 0.25, execute: work)
    }
}

private struct SelectedRowGlass: View {
    @ObservedObject private var windowKey = WindowKeyState.shared
    // Workspace-color dependency: the glass tint follows the App color, and
    // this leaf has no store dependency to re-run its body on a tint change.
    @ObservedObject private var tint = AppTintModel.shared
    @Environment(\.colorScheme) private var colorScheme

    var body: some View {
        // The native glass, untouched in dark mode: its own rim highlight
        // and depth are the look. Light mode's near-white chip needs a thin
        // muted rim to separate from the light sidebar (dark gets .clear).
        ZStack {
            if #available(macOS 26.0, *) {
                // Light mode: the solid-white chip's native drop shadow gets
                // truncated by the row clip into ragged edges, so clip the
                // glass to the chip (the muted rim below carries the edge).
                // Dark mode keeps the unclipped native glass, shadow and all
                // — it's invisible against the dark sidebar and clipping it
                // flattened the look.
                if colorScheme == .light {
                    Color.clear
                        .glassEffect(
                            .regular.tint(Theme.activeRowGlassTint),
                            in: RoundedRectangle(cornerRadius: 9, style: .continuous)
                        )
                        .clipShape(RoundedRectangle(cornerRadius: 9, style: .continuous))
                        // Fresh glass per focus transition (see WindowKeyState).
                        .id(windowKey.generation)
                } else {
                    Color.clear
                        .glassEffect(
                            .regular.tint(Theme.activeRowGlassTint),
                            in: RoundedRectangle(cornerRadius: 9, style: .continuous)
                        )
                        .id(windowKey.generation)
                }
            }
            RoundedRectangle(cornerRadius: 9, style: .continuous)
                .strokeBorder(
                    Color(
                        light: NSColor(hex: 0x000000, opacity: 0.12),
                        dark: NSColor.clear
                    ),
                    lineWidth: 1
                )
        }
        .transition(.identity)
    }
}

private extension View {
    /// Real Liquid Glass behind the selected session row; the flat fill
    /// covers pre-26. Rendered as a background rather than wrapping `self`
    /// in a branch: branching swaps the row's view identity on every
    /// selection change, re-inserting the row content with a visible fade.
    /// The top-lit rim stroke keeps the chip legible when the adaptive
    /// glass would otherwise disappear against the sidebar backdrop.
    @ViewBuilder
    func selectedRowGlass(_ active: Bool) -> some View {
        background {
            if active {
                SelectedRowGlass()
            }
        }
    }
}

/// Small pill button for the inline confirm row. Destructive styling
/// follows .session-archive-action.confirming (ProjectItem.svelte:2745-2756):
/// danger text on danger-15% (25% hovered); Cancel is muted on fg-10%.
private struct ConfirmPillButton: View {
    let label: String
    let destructive: Bool
    let action: () -> Void

    @State private var hovering = false

    var body: some View {
        Button(action: action) {
            Text(label)
                .font(.system(size: 11, weight: .medium))
                .foregroundStyle(
                    destructive
                        ? Theme.danger
                        : (hovering ? Theme.foreground : Theme.mutedForeground)
                )
                .padding(.horizontal, 8)
                .frame(height: 20)
                .background(
                    RoundedRectangle(cornerRadius: 6, style: .continuous)
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

/// Esc / click-away dismissal for the inline confirm row, mirroring the
/// Svelte window-click handler (ProjectItem.svelte:1019-1024). An invisible
/// background NSView the size of the row installs local monitors while the
/// confirm is visible: any mouse-down outside the row's window frame (or in
/// another window) cancels; Esc cancels and is swallowed.
struct RemoveConfirmDismissMonitor: NSViewRepresentable {
    let onCancel: () -> Void

    func makeNSView(context _: Context) -> MonitorView {
        let view = MonitorView()
        view.onCancel = onCancel
        return view
    }

    func updateNSView(_ view: MonitorView, context _: Context) {
        view.onCancel = onCancel
    }

    final class MonitorView: NSView {
        var onCancel: (() -> Void)?
        private var monitors: [Any] = []

        override func hitTest(_: NSPoint) -> NSView? { nil }

        override func viewDidMoveToWindow() {
            super.viewDidMoveToWindow()
            removeMonitors()
            guard window != nil else { return }
            if let mouse = NSEvent.addLocalMonitorForEvents(
                matching: [.leftMouseDown, .rightMouseDown]
            ) { [weak self] event in
                guard let self else { return event }
                if event.window !== self.window {
                    self.onCancel?()
                } else {
                    let rowFrame = self.convert(self.bounds, to: nil)
                    if !rowFrame.contains(event.locationInWindow) {
                        self.onCancel?()
                    }
                }
                return event
            } {
                monitors.append(mouse)
            }
            let keyHandler: (NSEvent) -> NSEvent? = { [weak self] event in
                if event.keyCode == 53 { // Esc
                    self?.onCancel?()
                    return nil
                }
                return event
            }
            if let key = NSEvent.addLocalMonitorForEvents(
                matching: .keyDown, handler: keyHandler
            ) {
                monitors.append(key)
            }
        }

        private func removeMonitors() {
            for monitor in monitors { NSEvent.removeMonitor(monitor) }
            monitors = []
        }

        deinit {
            // Monitors are normally removed in viewDidMoveToWindow(nil) on
            // unmount; this is the safety net. NSViews live on main.
            MainActor.assumeIsolated {
                for monitor in monitors { NSEvent.removeMonitor(monitor) }
            }
        }
    }
}

// MARK: - Busy project-name shimmer (ProjectItem.svelte:2083-2102)

/// Sidebar footer strip: settings ⚙ + add-project ＋ on the left,
/// collapse-all on the right (disabled while nothing is expanded, exactly
/// like the Svelte button binding `disabled={$expandedProjectIds.size===0}`).
/// The gear opens the full-screen settings view, matching the Svelte
/// footer's `onOpenSettings()` (Sidebar.svelte:565-567). Its old utility
/// menu items live in Settings → Advanced; Quit moved to the app menu.
struct SidebarFooter: View {
    /// False while a remote Host is selected: the session-tree local verbs
    /// (Add Project) disappear. Workspace identity/switching lives in the
    /// footer dots (an overlay the caller centers over this strip), so the
    /// footer button no longer doubles as a scope indicator.
    var localVerbsVisible = true
    let onAddProject: () -> Void
    let onOpenSettings: () -> Void
    let onAddWorkspace: () -> Void

    @State private var addMenuPresented = false

    var body: some View {
        HStack(spacing: 2) {
            if RemoteHostFeature.pickerEnabled {
                // One "+" for both create verbs. Add Project is a
                // Controller-local filesystem verb, so its row hides while a
                // remote Host is scoped; Add Workspace always applies.
                FooterButton(icon: .addProjectPlus, help: "Add project or workspace") {
                    addMenuPresented = true
                }
                // arrowEdge .top = the arrow sits on the button's top edge,
                // so the menu opens upward, above the footer.
                .popover(isPresented: $addMenuPresented, arrowEdge: .top) {
                    VStack(alignment: .leading, spacing: 2) {
                        if localVerbsVisible {
                            FooterAddMenuRow(
                                icon: "folder.badge.plus",
                                label: "Add Project…"
                            ) {
                                addMenuPresented = false
                                onAddProject()
                            }
                        }
                        FooterAddMenuRow(
                            icon: "rectangle.stack.badge.plus",
                            label: "Add Workspace…"
                        ) {
                            addMenuPresented = false
                            onAddWorkspace()
                        }
                    }
                    .padding(8)
                    .frame(width: 200)
                }
            } else if localVerbsVisible {
                // Release builds (no Host picker, no dots): the plain
                // Add Project verb.
                FooterButton(icon: .addProjectPlus, help: "Add Project", action: onAddProject)
            }
            Spacer()
            // Collapse-all moved to the menu bar (Session ▸ Collapse All
            // Folders, ⌥⌘B) — the footer keeps only "+" / dots / settings.
            FooterButton(icon: .settings, help: "Settings (⌘,)", action: onOpenSettings)
        }
        .padding(EdgeInsets(top: 0, leading: 13, bottom: 9.5, trailing: 13))
    }
}

/// One hover-highlighted row in the footer "+" popover. A tap row rather
/// than a Button, matching the workspace picker's rows — a plain Button
/// draws a keyboard focus ring inside the popover.
private struct FooterAddMenuRow: View {
    let icon: String
    let label: String
    let action: () -> Void

    @State private var hovering = false

    var body: some View {
        HStack(spacing: 8) {
            Image(systemName: icon)
                .font(.system(size: 11))
                .foregroundStyle(Theme.mutedForeground)
                .frame(width: 18)
            Text(label)
                .font(Theme.rowLabelFont)
                .foregroundStyle(Theme.foreground)
            Spacer(minLength: 8)
        }
        .padding(.horizontal, 7)
        .frame(height: 30)
        .contentShape(Rectangle())
        .background(
            RoundedRectangle(cornerRadius: 7, style: .continuous)
                .fill(hovering ? Theme.hoverRow : .clear)
        )
        .onHover { hovering = $0 }
        .onTapGesture(perform: action)
    }
}

private struct FooterButton: View {
    let icon: ChromeIcon
    /// Optional text next to the icon (the remote button shows the selected
    /// Host's name off-Local); nil keeps the square icon-only button.
    var label: String?
    var help: String = ""
    /// Optional foreground tint; the connected-Host button renders green.
    var tint: Color?
    var disabled: Bool = false
    var action: (() -> Void)?

    @State private var hovering = false

    var body: some View {
        Button {
            action?()
        } label: {
            // Footer icons render at 14px (Sidebar.svelte:811-814).
            HStack(spacing: 5) {
                ChromeIconView(icon: icon, size: 14)
                if let label {
                    Text(label)
                        .font(.system(size: 11, weight: .medium))
                        .lineLimit(1)
                }
            }
            .foregroundStyle(tint ?? Theme.mutedForeground)
            .opacity(disabled ? 0.4 : 1)
            .padding(.horizontal, label == nil ? 0 : 7)
            .frame(width: label == nil ? 22 : nil, height: 22)
            .background(
                RoundedRectangle(cornerRadius: 7, style: .continuous)
                    .fill(hovering && !disabled ? Theme.hoverRow : .clear)
            )
            .contentShape(Rectangle())
        }
        .buttonStyle(.plain)
        .disabled(disabled)
        .onHover { hovering = $0 }
        .help(help)
        .animation(.easeInOut(duration: 0.12), value: hovering)
    }
}

/// Keeps the sidebar's ordinary one-line tail truncation at rest, then glides
/// an overflowing title through the same viewport while its row is hovered.
/// The moving copy is edge-masked so it appears to pass beneath the row rather
/// than being sharply clipped at either side.
private struct HoverMarqueeSessionTitle: View {
    let title: String
    let isHovering: Bool

    init(_ title: String, isHovering: Bool) {
        self.title = title
        self.isHovering = isHovering
    }

    var body: some View {
        Text(title)
            .font(Theme.sessionLabelFont)
            .lineLimit(1)
            .truncationMode(.tail)
            // Keep this Text as the layout authority. The hover copy lives in
            // an overlay, so starting a marquee never moves adjacent controls.
            .opacity(isHovering ? 0 : 1)
            .overlay {
                if isHovering {
                    GeometryReader { viewport in
                        HoverMarqueeSessionTitleTrack(
                            title: title,
                            viewportWidth: viewport.size.width
                        )
                    }
                    .accessibilityHidden(true)
                }
            }
            .clipped()
            .accessibilityLabel(title)
    }
}

private struct HoverMarqueeSessionTitleTrack: View {
    let title: String
    let viewportWidth: CGFloat

    @State private var offset: CGFloat = 0

    private static let pointsPerSecond: CGFloat = 28
    private static let initialPause: UInt64 = 500_000_000
    private static let endPause: UInt64 = 900_000_000
    private static let fadeWidth: CGFloat = 10

    private var titleWidth: CGFloat {
        ceil(
            (title as NSString).size(
                withAttributes: [
                    .font: NSFont.systemFont(ofSize: 13, weight: .regular),
                ]
            ).width
        )
    }

    private var overflow: CGFloat {
        max(0, titleWidth - viewportWidth)
    }

    private var animationIdentity: String {
        // Geometry can settle over a few sub-pixel passes while the hover
        // actions appear. Pixel rounding avoids restarting for visual noise.
        "\(title)|\(Int(viewportWidth.rounded()))"
    }

    var body: some View {
        Text(title)
            .font(Theme.sessionLabelFont)
            .lineLimit(1)
            .fixedSize(horizontal: true, vertical: false)
            .offset(x: offset)
            .frame(width: viewportWidth, alignment: .leading)
            .mask(edgeFade)
            .task(id: animationIdentity) {
                offset = 0
                guard overflow > 1 else { return }

                let travelDuration = max(1.2, Double(overflow / Self.pointsPerSecond))
                while !Task.isCancelled {
                    try? await Task.sleep(nanoseconds: Self.initialPause)
                    guard !Task.isCancelled else { return }

                    withAnimation(.linear(duration: travelDuration)) {
                        offset = -overflow
                    }
                    try? await Task.sleep(
                        nanoseconds: UInt64(travelDuration * 1_000_000_000) + Self.endPause
                    )
                    guard !Task.isCancelled else { return }

                    withAnimation(.linear(duration: travelDuration)) {
                        offset = 0
                    }
                    try? await Task.sleep(
                        nanoseconds: UInt64(travelDuration * 1_000_000_000)
                    )
                }
            }
    }

    private var edgeFade: some View {
        HStack(spacing: 0) {
            if offset < -0.5 {
                LinearGradient(
                    colors: [.clear, .black],
                    startPoint: .leading,
                    endPoint: .trailing
                )
                .frame(width: Self.fadeWidth)
            } else {
                Color.black.frame(width: Self.fadeWidth)
            }

            Color.black

            if overflow > 1, offset > -overflow + 0.5 {
                LinearGradient(
                    colors: [.black, .clear],
                    startPoint: .leading,
                    endPoint: .trailing
                )
                .frame(width: Self.fadeWidth)
            } else {
                Color.black.frame(width: Self.fadeWidth)
            }
        }
    }
}
