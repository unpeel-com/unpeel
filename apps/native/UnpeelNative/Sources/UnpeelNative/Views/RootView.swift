//
//  RootView.swift
//  UnpeelNative
//
//  App shell: resizable sidebar (220–520, default 300) + content area,
//  each over its own vibrancy material (DESIGN.md §1/§4).
//
//  Sidebar collapse matches App.svelte: a 28×28 toggle button fixed at
//  window (72, 1) next to the traffic lights (App.svelte:1757-1778);
//  when collapsed a "+ new session" sibling appears at (104, 1)
//  (App.svelte:1442-1451, 1791-1808). Width animates 0.15s (the Svelte
//  sidebar width transition, Sidebar.svelte:619). ⌘B toggles (native
//  addition — the Svelte app has no shortcut). State persists to
//  UserDefaults next to unpeel.sidebar.width.
//

import SwiftUI

struct RootView: View {
    @ObservedObject var store: UnpeelStore
    @ObservedObject var cache: SurfaceCache

    @AppStorage("unpeel.sidebar.width") private var savedSidebarWidth: Double = Double(Theme.sidebarDefaultWidth)
    /// Live value during a resizer drag — plain @State so each drag frame
    /// skips the UserDefaults write + change-notification storm; the width
    /// persists once in onEnded.
    @State private var draggingSidebarWidth: Double?
    @State private var dragStartWidth: Double?
    /// Hover state for the sidebar resizer's grip capsule.
    @State private var resizerHovering = false

    /// Right project panel width — same resize/persist mechanics as the left
    /// sidebar (mirrored drag math in `projectSidebarResizer`), but PER ROOT
    /// PROJECT: each project's panel remembers its own width. The old shared
    /// key is the fallback for projects never resized, so existing installs
    /// keep their width.
    @AppStorage("unpeel.projectSidebar.width") private var sharedProjectSidebarWidth: Double = Double(Theme.sidebarDefaultWidth)
    @State private var projectSidebarWidths: [String: Double] =
        RootView.loadProjectSidebarWidths()
    @State private var draggingProjectSidebarWidth: Double?
    @State private var projectSidebarDragStartWidth: Double?
    @State private var projectSidebarResizerHovering = false

    private static let projectSidebarWidthsKey = "unpeel.projectSidebar.widths"

    private static func loadProjectSidebarWidths() -> [String: Double] {
        let raw = AppDefaults.shared.dictionary(forKey: projectSidebarWidthsKey) ?? [:]
        return raw.compactMapValues { $0 as? Double }
    }

    private static func saveProjectSidebarWidths(_ widths: [String: Double]) {
        if widths.isEmpty {
            AppDefaults.shared.removeObject(forKey: projectSidebarWidthsKey)
        } else {
            AppDefaults.shared.set(widths, forKey: projectSidebarWidthsKey)
        }
    }

    /// The width key follows the panel's own scoping: per root project (and
    /// per scope, since scoped project ids live in their Host's namespace).
    private var projectSidebarWidthKey: String {
        store.activeRootProjectID ?? "default"
    }
    /// Detached session-row drag ("Dia feel", SidebarSessionDrag.swift).
    /// Owned HERE — not in SidebarView — because the floating card renders in
    /// a window-level overlay below, so it can cross the sidebar edge and
    /// draw above the content pane's Metal-backed terminal (a sidebar-panel
    /// overlay clipped under it). Plain `@State` on purpose: a stable
    /// reference with NO subscription, so per-mouse-move published updates
    /// re-run only the tiny card overlay, never this layout body.
    @State private var sessionDragController = SidebarSessionDragController()

    private var sidebarWidth: Double {
        draggingSidebarWidth ?? savedSidebarWidth
    }

    private var shownSidebarWidth: CGFloat {
        store.sidebarCollapsed ? 0 : CGFloat(sidebarWidth)
    }

    private var projectSidebarWidth: Double {
        draggingProjectSidebarWidth
            ?? projectSidebarWidths[projectSidebarWidthKey]
            ?? sharedProjectSidebarWidth
    }

    /// Panel visibility is derived, never toggled: non-empty sidebar-group
    /// membership for the current root project shows it, and switching to a
    /// project without members hides it. Full-content pages (settings) cover
    /// the workspace, so the panel slides away with them.
    private var projectSidebarShown: Bool {
        !store.projectSidebarSessions.isEmpty && !store.settingsVisible
    }

    private var shownProjectSidebarWidth: CGFloat {
        projectSidebarShown ? CGFloat(projectSidebarWidth) : 0
    }

    /// The surface inset persists through a sidebar collapse — the content
    /// pane keeps floating in the frame (a leading gap appears in its place
    /// so the surface is framed on all four sides).
    private var surfaceInset: CGFloat { Theme.surfaceInset }

    var body: some View {
        // Settings is NOT a layout swap (the old whole-layout opacity fade
        // flashed the Metal-backed terminal surface). The app layout always
        // stays mounted; while settings is open the sidebar's list area
        // slides to the settings nav (SidebarView) and the content pane
        // swaps to the settings panel without animation (ContentArea).
        appLayout
            // Pane-title chips use the same detached Session drag controller
            // as sidebar rows, so they can sort the split or land at an exact
            // sidebar insertion gap with one continuous gesture.
            .environment(\.sidebarSessionDragController, sessionDragController)
            // Layout measures from the window top (DESIGN.md: 38px custom
            // titlebar inside the content), not from the AppKit titlebar
            // inset. Applied after the overlays so the toggle button lands
            // at (72, 1). Color scheme is inherited from the window
            // appearance (NSApp.appearance, driven by ThemePreference) —
            // no .preferredColorScheme override.
            .ignoresSafeArea()
        // Deferred background-pool spin-up: the pool's first reconcile
        // (gateway children, remote dials) starts ~2s after this layout
        // first appears, so startup work never competes with first paint.
        .onAppear {
            store.startWorkspacePoolAfterFirstPaint()
            sessionDragController.setProjectSidebarWidth(
                CGFloat(projectSidebarWidth)
            )
        }
        .onChange(of: projectSidebarWidth) { width in
            // The green Pin preview must match this root project's persisted
            // panel width, including live per-project switches and resizing.
            sessionDragController.setProjectSidebarWidth(CGFloat(width))
        }
        .onReceive(store.$nodes) { nodes in
            pruneSurfaceCache(
                nodes: nodes,
                prewarmedIDs: store.prewarmSessionIDs
            )
        }
        // A hover sweep changes only this array; it does not rebuild `nodes`.
        // Prune on that edge too, or panes that fall out of the three-slot
        // prewarm window retain duplicate unpeel-attach children indefinitely.
        .onReceive(store.$prewarmSessionIDs) { prewarmedIDs in
            pruneSurfaceCache(
                nodes: store.nodes,
                prewarmedIDs: prewarmedIDs
            )
        }
        // The detached session-drag floating card is NOT a SwiftUI overlay
        // here anymore: the drag controller hosts it in a borderless child
        // NSWindow so per-mouse-move positioning bypasses SwiftUI entirely
        // (cursor-locked, and it still renders above the Ghostty Metal
        // surface and every window chrome).
        // ⌃Tab MRU switcher — visible only while ⌃ is held mid-cycle.
        .overlay {
            if store.sessionSwitcher != nil {
                SessionSwitcherOverlay(store: store)
                    .transition(.opacity)
                    .zIndex(70)
            }
        }
        // ⌘K command palette.
        .overlay {
            if store.commandPaletteVisible {
                CommandPaletteOverlay(store: store)
                    .transition(.opacity)
                    .zIndex(80)
            }
        }
        // Transient toasts (e.g. "iPhone connected"), above the layout.
        .overlay {
            ToastOverlayView().zIndex(90)
        }
    }

    private func pruneSurfaceCache(
        nodes: [ProjectNode],
        prewarmedIDs: [String]
    ) {
        // Drop retained surfaces whose sessions are gone/exited and trim the
        // rest to the LRU budget (selected + pre-warmed panes are protected).
        // Use the canonical index as well as the rendered tree: app-state
        // decoding can be transiently incomplete during a rescan, and that
        // must not kill a still-live terminal pane.
        var live = Set(store.sessionsByID.values.filter(\.isLive).map(\.id))
        func walk(_ node: ProjectNode) {
            for session in node.sessions where session.isLive {
                live.insert(session.id)
            }
            node.worktrees.forEach(walk)
        }
        nodes.forEach(walk)
        // The right project panel mounts its members' surfaces; protect them
        // from the LRU sweep like pre-warmed panes so switching main sessions
        // never evicts a live panel terminal.
        let protectedIDs = prewarmedIDs + store.projectSidebarSessions.map(\.id)
        cache.prune(
            keeping: live,
            selectedID: store.selectedSessionID,
            prewarmedIDs: protectedIDs,
            sessionsDir: store.scopedSessionsDir
        )
    }

    // MARK: - Workspace layout (sidebar + content)

    private var appLayout: some View {
        ZStack(alignment: .topLeading) {
            // One continuous frame backdrop spans the window: behind the
            // sidebar, the content pane's rounded leading-corner notches,
            // AND a translucent terminal. One surface means the rounded
            // corners never expose a differently-shaded band (Appearance ▸
            // Transparency's "Frame" opacity is this view's opacity).
            SidebarBackground()

            HStack(spacing: 0) {
                // Inner content keeps its layout width; the outer frame clips
                // so the sidebar slides out behind the content pane.
                ZStack(alignment: .trailing) {
                    SidebarView(
                        store: store,
                        sessionDragController: sessionDragController
                    )
                    .frame(width: CGFloat(sidebarWidth))
                }
                .frame(width: shownSidebarWidth, alignment: .trailing)
                .clipped()

                ContentArea(
                    store: store,
                    selection: store.sessionSelection,
                    cache: cache
                )
                    .frame(maxWidth: .infinity, maxHeight: .infinity)
                    // Leave the corner-radius-wide leading strip to the shared
                    // sidebar backdrop. The content itself covers the strip
                    // except where its rounded corners cut away. Clipped to
                    // the same pane shape as the content, so the backdrop
                    // never pokes square corners into the surface inset gap.
                    .background(
                        ContentBackground()
                            .padding(.leading, Theme.windowCornerRadius)
                            .clipShape(Theme.contentPaneShape(inset: surfaceInset))
                    )
                    // Float the Surface: a sliver of the Background backdrop
                    // frames the content pane's top/right/bottom edges (the
                    // sidebar frames the left while open; collapsed, a
                    // matching leading gap takes its place).
                    .padding(.top, surfaceInset)
                    // Keep the trailing inset even while the project panel is
                    // shown: its pane cards are separate floating surfaces, so
                    // the frame gap must show between them and the main pane.
                    .padding(.trailing, surfaceInset)
                    .padding(.bottom, surfaceInset)
                    .padding(.leading, store.sidebarCollapsed ? surfaceInset : 0)
                    // With `isMovable` off, the frame/inset gaps around the
                    // content pane need an explicit drag surface. Behind the
                    // padded cell, so it peeks through the gaps (and any
                    // non-interactive fall-through) without shadowing
                    // interactive content.
                    .background(WindowDragArea())

                // Right project panel: sessions filed via "Move to Project
                // Sidebar", stacked as terminal panes. Derived visibility —
                // it slides out behind the content pane exactly like the left
                // sidebar when the current project has no members.
                ZStack(alignment: .leading) {
                    ProjectSidebarView(store: store, cache: cache)
                        .frame(width: CGFloat(projectSidebarWidth))
                        // Thin muted divider in the gap between the main
                        // pane and the panel's session stack. Its top tracks
                        // the PANES' top edge: in collapsed mode the panes
                        // slide down under the title strip and the divider
                        // follows, staying flush with the cards.
                        .overlay(alignment: .leading) {
                            // Inset a touch past the pane edges so the line
                            // reads as a quiet separator, not a full-bleed
                            // border.
                            Rectangle()
                                .fill(Theme.contentHairline)
                                .frame(width: 1)
                                .padding(
                                    .top,
                                    Theme.surfaceInset + 3
                                        + (store.sidebarCollapsed ? Theme.titleStripHeight : 0)
                                )
                                .padding(.bottom, Theme.surfaceInset + 3)
                                .allowsHitTesting(false)
                        }
                }
                .frame(width: shownProjectSidebarWidth, alignment: .leading)
                .clipped()
            }
        }
        .animation(
            .timingCurve(0.25, 0.1, 0.25, 1, duration: 0.15),
            value: store.sidebarCollapsed
        )
        .animation(
            .timingCurve(0.25, 0.1, 0.25, 1, duration: 0.15),
            value: projectSidebarShown
        )
        // Collapsed-sidebar breadcrumb strip — WINDOW chrome, not content
        // chrome, so the centered title and the trailing Open-in/site chips
        // span the main column and the project panel alike. TerminalArea
        // keeps a matching-height spacer. Under the resizers so the 8pt
        // edge handles still win at the strip's ends.
        .overlay(alignment: .top) {
            if store.sidebarCollapsed, !store.settingsVisible,
               !store.recentActivityVisible, store.archivedProjectID == nil {
                WorkspaceTitleStrip(store: store)
                    .padding(.top, surfaceInset)
            }
        }
        .overlay(alignment: .topLeading) {
            if !store.sidebarCollapsed {
                resizer
            }
        }
        .overlay(alignment: .topTrailing) {
            if projectSidebarShown {
                projectSidebarResizer
            }
        }
        .overlay(alignment: .topLeading) { titlebarButtons }
    }

    // MARK: - Titlebar buttons (sidebar toggle + collapsed new-session)

    private var titlebarButtons: some View {
        HStack(spacing: 4) {
            TitlebarIconButton(
                icon: .sidebarToggle,
                help: store.sidebarCollapsed ? "Show sidebar (⌘B)" : "Hide sidebar (⌘B)"
            ) {
                store.sidebarCollapsed.toggle()
            }
            .keyboardShortcut("b", modifiers: .command)

            // Global across every cached workspace. Remote project trees are
            // indexed when their bootstrap changes, not during this render.
            TitlebarActivityMenuButton(
                model: store.globalActivityMenu,
                onSelect: { item in
                    store.revealGlobalActivitySession(
                        workspaceKey: item.workspaceKey,
                        sessionID: item.session.sessionID
                    )
                },
                // The persisted history page remains local-only; remote rows
                // are still all present in the dropdown itself.
                onShowAll: store.selectedHostScope == .local
                    ? { store.openRecentActivity() }
                    : nil
            )

            if store.selectedHostScope == .local {
                // ⇧⌘R jumps straight to the Local recent-activity page.
                Button("") { store.toggleRecentActivity() }
                    .keyboardShortcut("r", modifiers: [.command, .shift])
                    .buttonStyle(.plain)
                    .frame(width: 0, height: 0)
                    .opacity(0)
                    .accessibilityHidden(true)

            }

            if store.sidebarCollapsed {
                // While a full-content page covers the workspace, "+" (new
                // session) is the wrong verb — offer the way back instead.
                if store.settingsVisible || store.recentActivityVisible
                    || store.archivedProjectID != nil {
                    TitlebarIconButton(icon: .back, help: "Back to workspace") {
                        if store.settingsVisible { store.closeSettings() }
                        store.recentActivityVisible = false
                        store.archivedProjectID = nil
                    }
                } else {
                    CollapsedNewSessionControl(
                        store: store,
                        selection: store.sessionSelection
                    )
                }
            }
        }
        // Clear of the traffic lights, vertically centered in the 38px bar.
        // In fullscreen the traffic lights are hidden, so slide flush left.
        .offset(x: store.windowIsFullScreen ? 12 : 80, y: 5)
        .animation(.easeInOut(duration: 0.15), value: store.windowIsFullScreen)
    }

    /// 8px-wide hit area straddling the sidebar edge (DESIGN.md §4). The
    /// visible hairline lives in ContentArea so it follows the content
    /// pane's rounded leading corners; a centered grip capsule appears on
    /// hover (and stays while dragging) — the same affordance as the
    /// terminal split divider.
    private var resizer: some View {
        ZStack {
            if resizerHovering || draggingSidebarWidth != nil {
                Capsule()
                    .fill(Theme.paneDividerLineHover)
                    .frame(width: 3, height: 36)
                    .transition(.opacity)
            }
        }
        .frame(width: 8)
        .frame(maxHeight: .infinity)
        .contentShape(Rectangle())
        .offset(x: CGFloat(sidebarWidth) - 4)
        .animation(.easeInOut(duration: 0.12), value: resizerHovering)
        .gesture(
            DragGesture(minimumDistance: 1, coordinateSpace: .global)
                .onChanged { value in
                    let start = dragStartWidth ?? sidebarWidth
                    dragStartWidth = start
                    let proposed = start + value.translation.width
                    draggingSidebarWidth = min(
                        Double(Theme.sidebarMaxWidth),
                        max(Double(Theme.sidebarMinWidth), proposed)
                    )
                }
                .onEnded { _ in
                    if let final = draggingSidebarWidth {
                        savedSidebarWidth = final
                    }
                    draggingSidebarWidth = nil
                    dragStartWidth = nil
                }
        )
        .onHover { inside in
            resizerHovering = inside
            if inside {
                NSCursor.resizeLeftRight.push()
            } else {
                NSCursor.pop()
            }
        }
    }

    /// Mirror of `resizer` for the project panel's leading edge. Anchored to
    /// the window's trailing edge, so it offsets left by the panel width and
    /// dragging left (negative translation) grows the panel.
    private var projectSidebarResizer: some View {
        ZStack {
            if projectSidebarResizerHovering || draggingProjectSidebarWidth != nil {
                Capsule()
                    .fill(Theme.paneDividerLineHover)
                    .frame(width: 3, height: 36)
                    .transition(.opacity)
            }
        }
        .frame(width: 8)
        .frame(maxHeight: .infinity)
        .contentShape(Rectangle())
        .offset(x: -(CGFloat(projectSidebarWidth) - 4))
        .animation(.easeInOut(duration: 0.12), value: projectSidebarResizerHovering)
        .gesture(
            DragGesture(minimumDistance: 1, coordinateSpace: .global)
                .onChanged { value in
                    let start = projectSidebarDragStartWidth ?? projectSidebarWidth
                    projectSidebarDragStartWidth = start
                    let proposed = start - value.translation.width
                    draggingProjectSidebarWidth = min(
                        Double(Theme.sidebarMaxWidth),
                        max(Double(Theme.sidebarMinWidth), proposed)
                    )
                }
                .onEnded { _ in
                    if let final = draggingProjectSidebarWidth {
                        projectSidebarWidths[projectSidebarWidthKey] = final
                        Self.saveProjectSidebarWidths(projectSidebarWidths)
                    }
                    draggingProjectSidebarWidth = nil
                    projectSidebarDragStartWidth = nil
                }
        )
        .onHover { inside in
            projectSidebarResizerHovering = inside
            if inside {
                NSCursor.resizeLeftRight.push()
            } else {
                NSCursor.pop()
            }
        }
    }
}

/// The collapsed-sidebar window-chrome strip: centered project breadcrumb
/// + branch, Open-in/local-site chips at the trailing edge. Mounted by
/// RootView so it spans the main column AND the right project panel. A
/// leaf view on purpose — it observes the hot selection state so RootView
/// itself never re-renders per session switch.
private struct WorkspaceTitleStrip: View {
    @ObservedObject var store: UnpeelStore
    @ObservedObject private var selection: SessionSelectionState
    @ObservedObject private var branchState: TitlebarBranchState

    init(store: UnpeelStore) {
        self.store = store
        self.selection = store.sessionSelection
        self.branchState = store.titlebarBranchState
    }

    var body: some View {
        TitleBarView(
            segments: store.titlebarSegments,
            branch: branchState.presentation.name,
            branchIsWorktree: branchState.presentation.isWorktree,
            height: Theme.titleStripHeight,
            titleYOffset: -4
        )
        // Project-row Open-in/site verbs also live here as the classic
        // glass split chips, so they stay reachable with the sidebar
        // collapsed. Local-machine scopes only — a scoped local workspace's
        // paths are real paths on this Mac, so Open-in is just as valid
        // there; a true remote Host's paths never reach a local editor.
        .overlay(alignment: .trailing) {
            if store.selectedHostScope.isLocalMachine {
                HStack(spacing: 10) {
                    if let session = shownSession {
                        let urls = store.localSiteURLs(
                            forProjectFamilyOf: session.projectID
                        )
                        if !urls.isEmpty {
                            LocalSiteChip(urls: urls)
                        }
                    }
                    if let path = activeWorkspacePath {
                        WorkspaceOpenMenu(
                            preferredTarget: WorkspaceOpenTarget.preferred(
                                forEditor: store.codeEditor
                            ),
                            onOpen: { store.openWorkspace(path: path, in: $0) }
                        )
                    }
                }
                .padding(.trailing, Theme.surfaceInset)
                // Ride up with the title so chips sit level with the
                // traffic lights in the tighter collapsed strip.
                .offset(y: -2)
            }
        }
    }

    private var shownSession: SessionEntry? {
        selection.sessionID.flatMap { store.displaySessionsByID[$0] }
    }

    /// Workspace path behind the Open-in menu. Local-machine scopes resolve
    /// through the display projection; a true remote Host's paths are never
    /// offered to a local editor.
    private var activeWorkspacePath: String? {
        guard store.selectedHostScope.isLocalMachine else { return nil }
        if let session = shownSession {
            return session.worktreePath
                ?? store.displayProjectsByID[session.projectID]?.path
        }
        return store.displayNodes.first?.project.path
    }
}

/// Selection-aware leaf for the collapsed-sidebar "+" button. RootView owns
/// the whole window layout and must not observe hot session selection state;
/// only this control needs to retarget when the selected project changes.
private struct CollapsedNewSessionControl: View {
    @ObservedObject var store: UnpeelStore
    @ObservedObject var selection: SessionSelectionState

    var body: some View {
        if let projectID = activeProjectID {
            TitlebarNewSessionMenu(
                menuPresets: store.displayAvailablePresets,
                onLaunch: { preset in
                    store.launchSession(
                        projectID: projectID,
                        command: preset.command,
                        sourcePresetID: preset.command.isEmpty ? nil : preset.id
                    )
                },
                onManagePresets: { store.openSettings(tab: .presets) },
                addableApps: store.selectedHostScope == .local ? store.addableApps : [],
                onAddApp: store.selectedHostScope == .local ? { store.addAppPreset($0) } : nil
            )
        }
    }

    private var activeProjectID: String? {
        if let id = selection.sessionID,
           let session = store.displaySessionsByID[id] {
            return session.projectID
        }
        return store.displayNodes.first?.project.id
    }
}

/// 28×28 radius-6 titlebar icon button (App.svelte .pane-titlebar-sidebar-toggle):
/// 16px icon, muted → foreground on hover, fg-10% hover background.
struct TitlebarIconButton: View {
    let icon: ChromeIcon
    var help: String = ""
    let action: () -> Void

    @State private var hovering = false

    var body: some View {
        Button(action: action) {
            ChromeIconView(icon: icon, size: 16)
                .foregroundStyle(hovering ? Theme.foreground : Theme.mutedForeground)
                .frame(width: 28, height: 28)
                .background(
                    RoundedRectangle(cornerRadius: 6, style: .continuous)
                        .fill(hovering ? Theme.hoverRow : .clear)
                )
        }
        .buttonStyle(.plain)
        .compatFocusEffectDisabled()
        .onHover { hovering = $0 }
        .help(help)
        .animation(.easeInOut(duration: 0.12), value: hovering)
    }
}

/// Collapsed-sidebar "new session" plus (App.svelte:1449), 16px in a 28×28
/// button. Matches `TitlebarIconButton`'s hover treatment (radius-6 hoverRow
/// chip, foreground on hover). Because `.onHover` does not fire over a `Menu`
/// label on macOS, hover is reported by a `HoverReporter` tracking view behind
/// the menu — the same pattern as `SidebarView.newSessionMenu`.
struct TitlebarNewSessionMenu: View {
    let menuPresets: [Preset]
    let onLaunch: (Preset) -> Void
    let onManagePresets: () -> Void
    var addableApps: [InstalledAppInfo] = []
    var onAddApp: ((InstalledAppInfo) -> Void)?

    @State private var hovering = false

    var body: some View {
        Menu {
            newSessionMenuContent(
                menuPresets: menuPresets,
                onLaunch: onLaunch,
                onManagePresets: onManagePresets,
                addableApps: addableApps,
                onAddApp: onAddApp
            )
        } label: {
            // Thin plusIcon, not the heavier plusBold (matches the project-item
            // "+" in SidebarView.newSessionMenu).
            ChromeIconView(icon: .plus, size: 16)
                .foregroundStyle(hovering ? Theme.foreground : Theme.mutedForeground)
                .frame(width: 28, height: 28)
        }
        .menuStyle(.borderlessButton)
        .menuIndicator(.hidden)
        .frame(width: 28, height: 28)
        .background(
            RoundedRectangle(cornerRadius: 6, style: .continuous)
                .fill(hovering ? Theme.hoverRow : .clear)
        )
        .background(HoverReporter { hovering = $0 })
        .animation(.easeInOut(duration: 0.12), value: hovering)
        .help("New session")
    }
}

/// The common activity-menu projection used by the titlebar and menu-bar
/// surfaces. Attention rows form their own section and win over the active or
/// unread buckets, so one session can never appear twice with conflicting
/// states (for example, as both Blocked and recently finished).
struct ActivityMenuSessions {
    let jobs: [SessionEntry]
    let blockers: [SessionEntry]
    let finished: [SessionEntry]

    init(
        nodes: [ProjectNode],
        allSessions: [SessionEntry],
        jobs: [SessionEntry],
        finished: [SessionEntry]
    ) {
        var blockerCandidates: [SessionEntry] = []
        func collectBlockers(_ nodes: [ProjectNode]) {
            for node in nodes {
                blockerCandidates.append(contentsOf: node.sessions.filter { $0.status == .attention })
                collectBlockers(node.worktrees)
            }
        }
        collectBlockers(nodes)

        // Keep the visible project-tree order first, then cover the transient
        // case where a live manifest exists before (or without) its project
        // node. Dictionary iteration is not stable, so orphan blockers use the
        // same lifecycle/id order as every Recent surface.
        let renderedBlockerIDs = Set(blockerCandidates.map(\.id))
        let orphanBlockers = allSessions.filter {
            $0.status == .attention && !renderedBlockerIDs.contains($0.id)
        }.sorted { lhs, rhs in
            // Every candidate is in Recent's non-working tier, so its shared
            // comparator reduces exactly to lifecycle descending, then id.
            let lhsStamp = max(lhs.createdAt, lhs.lifecycleAtMs ?? 0)
            let rhsStamp = max(rhs.createdAt, rhs.lifecycleAtMs ?? 0)
            if lhsStamp != rhsStamp { return lhsStamp > rhsStamp }
            return lhs.id < rhs.id
        }
        blockerCandidates.append(contentsOf: orphanBlockers)

        blockers = Self.uniqued(blockerCandidates)
        let blockerIDs = Set(blockers.map(\.id))
        self.jobs = Self.uniqued(jobs, excluding: blockerIDs)
        let shownIDs = blockerIDs.union(self.jobs.map(\.id))
        self.finished = Self.uniqued(finished, excluding: shownIDs)
    }

    var sectionCount: Int {
        [blockers, jobs, finished].reduce(into: 0) { count, sessions in
            if !sessions.isEmpty { count += 1 }
        }
    }

    private static func uniqued(
        _ sessions: [SessionEntry], excluding excluded: Set<String> = []
    ) -> [SessionEntry] {
        var seen = excluded
        return sessions.filter { seen.insert($0.id).inserted }
    }
}

/// The always-visible titlebar activity affordance: a braille spinner while
/// sessions are actively working, a glass bell otherwise — even when
/// everything is read. Blockers turn the glyph and badge orange. Opens the
/// activity dropdown (blocked + active + recently finished + the "All recent"
/// footer link); an unread dot rides the glyph while settled sessions carry
/// unread badges.
struct TitlebarActivityMenuButton: View {
    @ObservedObject var model: GlobalActivityMenuModel
    let onSelect: (GlobalActivityMenuItem) -> Void
    /// Footer link: opens the app-wide "All recent" page.
    let onShowAll: (() -> Void)?

    @State private var hovering = false
    @State private var showing = false

    private var activity: GlobalActivityMenuSessions { model.activity }

    private var glyphColor: Color {
        if !activity.blockers.isEmpty { return Theme.attention }
        if hovering || showing { return Theme.foreground }
        return Theme.genericSpinner
    }

    var body: some View {
        Button {
            model.refreshWorkspaceMetadata()
            showing.toggle()
        } label: {
            Group {
                if !activity.jobs.isEmpty {
                    TitlebarBrailleSpinner(color: glyphColor)
                } else {
                    ChromeIconView(icon: .bell, size: 16)
                        .foregroundStyle(glyphColor)
                        .frame(width: 16, height: 16)
                }
            }
            .frame(width: 28, height: 28)
            .overlay(alignment: .topTrailing) {
                if !activity.blockers.isEmpty || !activity.finished.isEmpty {
                    Circle()
                        .fill(activity.blockers.isEmpty ? Theme.unread : Theme.attention)
                        .frame(width: 6, height: 6)
                        .offset(x: -5, y: 5)
                }
            }
            .background(
                RoundedRectangle(cornerRadius: 6, style: .continuous)
                    .fill(hovering || showing ? Theme.hoverRow : .clear)
            )
        }
        .buttonStyle(.plain)
        .compatFocusEffectDisabled()
        .onHover { hovering = $0 }
        .help(activity.blockers.isEmpty ? "Recent activity" : "Session blocked")
        .animation(.easeInOut(duration: 0.12), value: hovering)
        .popover(isPresented: $showing, arrowEdge: .bottom) {
            ActivityMenuList(
                jobs: activity.jobs,
                blockers: activity.blockers,
                finished: activity.finished,
                onSelect: { item in
                    showing = false
                    onSelect(item)
                },
                onShowAll: onShowAll.map { action in {
                    showing = false
                    action()
                } }
            )
            .padding(6)
            .frame(width: 360)
        }
    }
}

/// Shared dropdown body for the activity menus — blockers, active jobs, then
/// recently-finished unread sessions, with dividers between non-empty groups.
/// Rendered identically by the in-app titlebar popover
/// (`TitlebarActivityMenuButton`) and the macOS menu-bar dropdown
/// (`MenuBarActivityPanel`) so the two surfaces never drift. Callers wrap it
/// in their own `.padding`/`.frame` and supply the lookups.
struct ActivityMenuList: View {
    /// Empty groups should still read as deliberate menu content instead of
    /// a cramped fallback line. Kept public to the module so the AppKit
    /// menu-bar popover can reserve the exact same height before it opens.
    static let emptyScrollHeight: CGFloat = 44

    let jobs: [GlobalActivityMenuItem]
    let blockers: [GlobalActivityMenuItem]
    let finished: [GlobalActivityMenuItem]
    let onSelect: (GlobalActivityMenuItem) -> Void
    /// Footer link to the app-wide "All recent" page.
    let onShowAll: (() -> Void)?

    private var rowCount: Int { jobs.count + blockers.count + finished.count }

    private var sectionCount: Int {
        [blockers, jobs, finished].reduce(into: 0) { count, rows in
            if !rows.isEmpty { count += 1 }
        }
    }

    /// Deterministic sizing keeps both NSPopover and SwiftUI stable. Large
    /// fleets cap at ten visible rows and lazily scroll the rest.
    private var scrollHeight: CGFloat {
        guard rowCount > 0 else { return Self.emptyScrollHeight }
        let dividers = CGFloat(max(0, sectionCount - 1)) * 9
        return min(CGFloat(rowCount) * 42 + dividers, 429)
    }

    var body: some View {
        VStack(alignment: .leading, spacing: 2) {
            ScrollView {
                LazyVStack(alignment: .leading, spacing: 2) {
                    if rowCount == 0 {
                        Text("No active sessions")
                            .font(.system(size: 13))
                            .foregroundStyle(Theme.mutedForeground.opacity(0.72))
                            .padding(.horizontal, 14)
                            .padding(.vertical, 12)
                            .frame(maxWidth: .infinity, alignment: .leading)
                    } else {
                        // Attention is actionable, so blockers always lead.
                        ForEach(blockers) { session in
                            row(session, unread: false, working: false, blocked: true)
                        }
                        if !blockers.isEmpty && (!jobs.isEmpty || !finished.isEmpty) {
                            Divider()
                                .padding(.horizontal, 8)
                                .padding(.vertical, 2)
                        }
                        ForEach(jobs) { session in
                            row(session, unread: false, working: true, blocked: false)
                        }
                        if !jobs.isEmpty && !finished.isEmpty {
                            Divider()
                                .padding(.horizontal, 8)
                                .padding(.vertical, 2)
                        }
                        ForEach(finished) { session in
                            row(session, unread: true, working: false, blocked: false)
                        }
                    }
                }
            }
            .frame(height: scrollHeight)
            if let onShowAll {
                AllRecentMenuRow(onSelect: onShowAll)
            }
        }
    }

    @ViewBuilder
    private func row(
        _ item: GlobalActivityMenuItem, unread: Bool, working: Bool, blocked: Bool
    ) -> some View {
        TitlebarActivityMenuRow(
            title: item.session.title,
            command: item.session.command,
            workspace: item.workspaceName,
            workspaceTint: item.workspaceTint,
            projectPath: item.session.projectPath,
            status: item.session.status,
            alertBody: item.session.alertBody,
            unread: unread,
            working: working,
            blocked: blocked
        ) {
            onSelect(item)
        }
    }
}

/// Footer row of the activity dropdowns: opens the app-wide "All recent"
/// history page. Always present, even when nothing is active or unread —
/// this is the bell's guaranteed path to history.
private struct AllRecentMenuRow: View {
    let onSelect: () -> Void

    @State private var hovering = false

    var body: some View {
        Button(action: onSelect) {
            HStack(spacing: 8) {
                Text("All recent")
                    .font(.system(size: 13))
                    .foregroundStyle(hovering ? Theme.foreground : Theme.mutedForeground)
                Spacer(minLength: 0)
                Image(systemName: "chevron.right")
                    .font(.system(size: 10, weight: .semibold))
                    .foregroundStyle(Theme.mutedForeground)
            }
            .padding(.horizontal, 14)
            .padding(.vertical, 6)
            .frame(maxWidth: .infinity, alignment: .leading)
            .contentShape(Rectangle())
            .background(
                RoundedRectangle(cornerRadius: 7, style: .continuous)
                    .fill(hovering ? Theme.hoverRow : .clear)
            )
        }
        .buttonStyle(.plain)
        .compatFocusEffectDisabled()
        .onHover { hovering = $0 }
    }
}

/// Single row in the activity dropdowns: the session's prompt-derived title over
/// its project name, with a subtle hover highlight. A working session shows the
/// CLI-colored braille loader on the left and the CLI logo on the right; a
/// settled session shows the CLI logo on the left and an unread dot on the right.
private struct TitlebarActivityMenuRow: View {
    let title: String
    /// Launch command, used to render the CLI/provider icon (claude, codex, …).
    let command: String
    let workspace: String
    let workspaceTint: AppTint
    let projectPath: String
    let status: String
    let alertBody: String?
    /// Recently-finished row: shows the #60a5fa unread dot in place of the
    /// transient status label.
    let unread: Bool
    /// Active job: braille loader (left) + CLI logo (right) instead of a static
    /// logo + unread dot.
    let working: Bool
    /// Attention row: orange dot (left) + explicit Blocked label (right).
    let blocked: Bool
    let onSelect: () -> Void

    @State private var hovering = false

    var body: some View {
        Button(action: onSelect) {
            HStack(alignment: .center, spacing: 8) {
                // Leading: colored loader while working, done dot when
                // finished, else the CLI logo.
                Group {
                    if working {
                        BrailleSpinner(color: Theme.toolSpinnerColor(forCommand: command))
                    } else if blocked {
                        Circle()
                            .fill(Theme.attention)
                            .frame(width: 7, height: 7)
                            .shadow(color: Theme.attention.opacity(0.55), radius: 3)
                    } else if unread {
                        Circle()
                            .fill(Theme.unread)
                            .frame(width: 7, height: 7)
                    } else {
                        ToolIconView(command: command, size: 16)
                    }
                }
                .frame(width: 16, height: 16)

                VStack(alignment: .leading, spacing: 1) {
                    Text(title)
                        .font(.system(size: 13))
                        .foregroundStyle(Theme.foreground)
                        .lineLimit(1)
                        .truncationMode(.tail)
                    Group {
                        if let alertBody, !alertBody.isEmpty {
                            Text("Alert · \(alertBody)")
                                .foregroundStyle(Theme.mutedForeground)
                                .truncationMode(.tail)
                        } else {
                            HStack(spacing: 4) {
                                Circle()
                                    .fill(workspaceTint.swatch)
                                    .frame(width: 6, height: 6)
                                Text(workspace)
                                    .foregroundStyle(Theme.mutedForeground)
                                Text("›")
                                    .foregroundStyle(Theme.mutedForeground.opacity(0.65))
                                Text(projectPath)
                                    .foregroundStyle(Theme.mutedForeground)
                                    .truncationMode(.tail)
                            }
                        }
                    }
                    .font(.system(size: 11))
                    .lineLimit(1)
                }
                Spacer(minLength: 0)

                // Trailing: CLI logo while working or finished, else status.
                if blocked {
                    Text("Blocked")
                        .font(.system(size: 11, weight: .semibold))
                        .foregroundStyle(Theme.attention)
                } else if unread, alertBody != nil {
                    Text("Alert")
                        .font(.system(size: 11, weight: .semibold))
                        .foregroundStyle(Theme.unread)
                } else if working || unread {
                    ToolIconView(command: command, size: 16)
                        .frame(width: 16, height: 16)
                } else if status == "Starting" || status == "Restarting" || status == "Resuming" {
                    Text(status)
                        .font(.system(size: 11))
                        .foregroundStyle(Theme.mutedForeground)
                }
            }
            .padding(.horizontal, 8)
            .padding(.vertical, 6)
            .frame(maxWidth: .infinity, alignment: .leading)
            .contentShape(Rectangle())
            .background(
                RoundedRectangle(cornerRadius: 7, style: .continuous)
                    .fill(hovering ? Theme.hoverRow : .clear)
            )
        }
        .buttonStyle(.plain)
        .compatFocusEffectDisabled()
        .onHover { hovering = $0 }
    }
}

/// Text-based titlebar spinner. The layer-backed sidebar spinner does not
/// reliably draw inside a SwiftUI Menu label, so this uses the same braille
/// frames with a lightweight TimelineView for the single titlebar affordance.
private struct TitlebarBrailleSpinner: View {
    let color: Color

    var body: some View {
        TimelineView(.periodic(from: .now, by: Theme.spinnerInterval)) { context in
            Text(frame(for: context.date))
                .font(.system(size: 14.7, weight: .bold, design: .monospaced))
                .foregroundStyle(color)
                .shadow(color: color.opacity(0.45), radius: 3)
                .frame(width: 16, height: 16)
        }
        .accessibilityHidden(true)
    }

    private func frame(for date: Date) -> String {
        let index = Int(date.timeIntervalSinceReferenceDate / Theme.spinnerInterval)
        return Theme.spinnerFrames[index % Theme.spinnerFrames.count]
    }
}

/// `.focusEffectDisabled()` is macOS 14+; on macOS 13 the focus ring these
/// chrome buttons suppress simply stays, so fall through unchanged.
extension View {
    @ViewBuilder
    func compatFocusEffectDisabled() -> some View {
        if #available(macOS 14.0, *) {
            focusEffectDisabled()
        } else {
            self
        }
    }
}
