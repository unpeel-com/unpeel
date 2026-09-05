//
//  TerminalArea.swift
//  UnpeelNative
//
//  Main content pane: the selected session's retained Ghostty surface,
//  running edge-to-edge to the window top while the sidebar is open (the
//  pane header is the only chrome). The compact title strip returns when
//  the sidebar is collapsed. Surfaces are swapped, never destroyed
//  (DESIGN.md §6).
//
//  While settings is open the pane swaps to SettingsContentHost. The swap
//  is deliberately INSTANT (`.transition(.identity)` + `.animation(nil)`):
//  the terminal surface is a Metal-backed NSView, and animating opacity
//  or frame across a CAMetalLayer flashes (the layer is snapshotted, the
//  drawable goes black for a frame, vibrancy re-composites) — that was the
//  root cause of the old settings blink. Removing the terminal from the
//  hierarchy without animation is the same flash-free path used when
//  switching to a dead/empty session; the pane itself stays retained in
//  SurfaceCache and its renderer pauses via viewDidMoveToWindow(nil).
//

import AppKit
import SwiftUI

/// Keeps asynchronous git-branch updates inside the title strip. The branch
/// is ancillary chrome; it must not invalidate ContentArea or the sidebar
/// after the selected Session has already rendered.
private struct SelectionTitleBarView: View {
    let segments: [String]
    let showsBranch: Bool
    @ObservedObject var branchState: TitlebarBranchState
    let height: CGFloat
    let titleYOffset: CGFloat

    var body: some View {
        TitleBarView(
            segments: segments,
            branch: showsBranch ? branchState.presentation.name : nil,
            branchIsWorktree: showsBranch && branchState.presentation.isWorktree,
            height: height,
            titleYOffset: titleYOffset
        )
    }
}

struct ContentArea: View {
    @ObservedObject var store: UnpeelStore
    @ObservedObject var selection: SessionSelectionState
    @ObservedObject private var viewerPresence = ViewerPresenceStore.shared
    @ObservedObject var cache: SurfaceCache
    @ObservedObject private var transparency = TransparencyModel.shared
    /// Workspace changes replace one Host-owned terminal surface with
    /// another. Never opacity-animate the CAMetalLayer itself (that flashes
    /// black on macOS); briefly cover it with the resolved terminal canvas
    /// and fade that cover away instead.
    @State private var workspaceTerminalFadeOpacity = 0.0
    @State private var workspaceTerminalFadeGeneration = 0
    /// A healthy workspace switch usually crosses `.connecting` for only a
    /// frame or two. Avoid flashing a stale-state banner for that ordinary
    /// transition; reveal it only when the connection actually takes time.
    @State private var delayedConnectingBannerVisible = false
    @State private var connectingBannerDelayGeneration = 0

    private static let connectingBannerDelay: TimeInterval = 1.5

    var body: some View {
        // Remote scope renders through the SAME workspace pane: the store's
        // display projection feeds it, and only the terminal byte transport
        // differs (the runtime's in-memory VT pane instead of a local
        // Ghostty surface).
        ZStack {
            if store.settingsVisible {
                SettingsContentHost(store: store)
                    .transition(.identity)
            } else {
                workspacePane
                    .transition(.identity)
            }
        }
        // Belt and braces: even if a future caller wraps settingsVisible in
        // withAnimation, this subtree must not animate the swap.
        .animation(nil, value: store.settingsVisible)
        // The shared content pane covers terminals, settings, archives, and
        // empty states, so rounding here keeps every right-hand view aligned.
        // Trailing radii track the collapsing surface inset.
        .clipShape(Theme.contentPaneShape(inset: Theme.surfaceInset))
        .overlay {
            // The same hairline the pane cards draw, for the full-content
            // screens (settings, libraries, launcher, empty/dead states).
            // Terminal mounts skip it — their cards already carry the rim,
            // and doubling it on the shared edge reads twice as strong.
            // Collapsed sidebar skips it too: those screens render as their
            // own rounded card below the strip, carrying their own rim.
            if store.settingsVisible || !showsTerminal, !store.sidebarCollapsed {
                Theme.contentPaneShape(inset: Theme.surfaceInset)
                    .strokeBorder(Theme.contentHairline, lineWidth: 1)
                    .allowsHitTesting(false)
            }
        }
        .onAppear {
            refreshConnectingBannerDelay()
            DispatchQueue.main.async {
                syncRemotePresentedSessions()
            }
        }
        .onChange(of: selection.sessionID) { _ in
            // Content reads the published selection directly, so it swaps in
            // that publication's render pass instead of mirroring through a
            // second @State update (formerly delayed another runloop turn).
            // Size normalization still defers its AppKit refit as required.
            normalizeShownTerminalSize()
        }
        .onChange(of: store.selectedHostScope) { _ in
            beginWorkspaceTerminalFade()
            refreshConnectingBannerDelay()
            DispatchQueue.main.async {
                syncRemotePresentedSessions()
            }
        }
        .onChange(of: store.remoteHostRuntime.connectionState) { _ in
            refreshConnectingBannerDelay()
        }
        .onChange(of: store.remoteHostRuntime.snapshot != nil) { _ in
            refreshConnectingBannerDelay()
        }
        .onChange(of: remotePresentedSessionIDs) { _ in
            syncRemotePresentedSessions()
        }
        .onDisappear {
            connectingBannerDelayGeneration &+= 1
            delayedConnectingBannerVisible = false
            store.remoteHostRuntime.setPresentedTerminalSessions([])
        }
        // Coming forward from the phone: re-assert the desktop grid on the
        // shown session (see normalizeShownTerminalSize).
        .onReceive(
            NotificationCenter.default.publisher(for: NSApplication.didBecomeActiveNotification)
        ) { _ in
            normalizeShownTerminalSize()
        }
        // A phone/remote viewer leaving is the "no longer mobile controlled"
        // moment — snap the grid back once nothing else owns the shared PTY.
        .onChange(of: shownSessionHasViewers) { hasViewers in
            if !hasViewers { normalizeShownTerminalSize() }
        }
    }

    /// Whether the workspace is scoped to a remote Host: same chrome, but
    /// the terminal transport is the runtime's in-memory VT pane and the
    /// Controller-local titlebar utilities (gallery, local-site links, open
    /// in editor) hide.
    private var isRemoteScope: Bool {
        store.selectedHostScope != .local
    }

    /// Errors and an interrupted established connection remain immediate.
    /// Only the ordinary `.connecting` transition is deferred because it is
    /// expected to disappear quickly during a workspace switch.
    private var contentConnectionBanner: RemoteConnectionPresentation.Banner? {
        let state = store.remoteHostRuntime.connectionState
        guard RemoteContentBannerPolicy.allowsContentBanner(
            state: state,
            isRemoteScope: isRemoteScope,
            connectingDelayElapsed: delayedConnectingBannerVisible
        ) else { return nil }
        return RemoteConnectionPresentation(
            state: state,
            hasSnapshot: store.remoteHostRuntime.snapshot != nil
        ).contentBanner
    }

    private func refreshConnectingBannerDelay() {
        connectingBannerDelayGeneration &+= 1
        let generation = connectingBannerDelayGeneration
        delayedConnectingBannerVisible = false
        guard RemoteContentBannerPolicy.shouldScheduleConnectingDelay(
            state: store.remoteHostRuntime.connectionState,
            hasSnapshot: store.remoteHostRuntime.snapshot != nil,
            isRemoteScope: isRemoteScope
        ) else { return }

        DispatchQueue.main.asyncAfter(deadline: .now() + Self.connectingBannerDelay) {
            guard connectingBannerDelayGeneration == generation,
                  RemoteContentBannerPolicy.shouldScheduleConnectingDelay(
                      state: store.remoteHostRuntime.connectionState,
                      hasSnapshot: store.remoteHostRuntime.snapshot != nil,
                      isRemoteScope: isRemoteScope
                  )
            else { return }
            delayedConnectingBannerVisible = true
        }
    }

    /// True while a phone or remote client is currently viewing the shown
    /// session (so it may be driving the shared PTY size on purpose).
    private var shownSessionHasViewers: Bool {
        guard let id = selection.sessionID else { return false }
        return !(viewerPresence.viewers[id]?.isEmpty ?? true)
    }

    /// Snap the shown session's hosted PTY back to the desktop's full grid
    /// when nothing else is driving it. A phone in fit-to-screen mode resizes
    /// the *shared* PTY smaller; Ghostty's own desktop surface stays
    /// full-size, so no resize event fires and the terminal keeps rendering
    /// narrow (content wraps early, dead space on the right) until the user
    /// nudges the window. Re-asserting requires the FULL resize path
    /// (`forceRefitNow`): the surface's own size never changed, so the
    /// drift-gated `refitNow` skips exactly this case — but forcing it on
    /// every trigger would put SIGWINCH repaint churn back into ordinary
    /// session switches. The viewer-presence latch splits the difference:
    /// only a session some remote viewer has actually been seen on gets the
    /// forced re-assert (once per sighting); everything else keeps the
    /// cheap drift check. No-op while phone-controlled (a live letterbox
    /// override or an active viewer owns the grid on purpose), so it never
    /// fights a phone that is still connected.
    private func normalizeShownTerminalSize() {
        guard !isRemoteScope else { return }
        guard !store.settingsVisible,
              store.archivedProjectID == nil,
              !store.recentActivityVisible,
              let id = selection.sessionID
        else { return }
        guard store.phoneResizeOverrides[id] == nil else { return }
        guard viewerPresence.viewers[id]?.isEmpty ?? true else { return }
        DispatchQueue.main.async {
            // Consume only when the pane can actually act on it — a pane
            // mid-swap (detached, window nil) keeps its candidacy for the
            // next trigger instead of losing the repair.
            guard let pane = cache.existingPane(for: id), pane.window != nil
            else { return }
            if ViewerPresenceStore.shared.consumeGridReassertCandidate(id) {
                pane.forceRefitNow()
            } else {
                pane.refitNow()
            }
        }
    }

    /// A fade-through-canvas workspace transition. This softens the Host
    /// swap without animating, snapshotting, or remounting a live Metal
    /// terminal. Session changes inside one workspace remain immediate.
    private func beginWorkspaceTerminalFade() {
        guard !SidebarMotion.reduceMotion, !store.settingsVisible else {
            workspaceTerminalFadeOpacity = 0
            return
        }
        workspaceTerminalFadeGeneration += 1
        let generation = workspaceTerminalFadeGeneration
        var covered = Transaction()
        covered.disablesAnimations = true
        withTransaction(covered) {
            workspaceTerminalFadeOpacity = 1
        }
        // Let the newly scoped terminal mount behind the opaque canvas, then
        // reveal it. A generation gate handles fast consecutive switches.
        DispatchQueue.main.async {
            guard workspaceTerminalFadeGeneration == generation else { return }
            withAnimation(.easeOut(duration: 0.2)) {
                workspaceTerminalFadeOpacity = 0
            }
        }
    }

    private var workspacePane: some View {
        VStack(spacing: 0) {
            // No content titlebar while the sidebar is open: terminal panes
            // run all the way to the window top and carry their own header
            // chrome. The strip comes back for the main-pane libraries (their
            // only title and window-drag surface) and a COLLAPSED sidebar —
            // the panes slide down and the current project/branch fades in
            // centered. RootView owns the visible strip; keep only the
            // matching height here.
            if coversTerminal {
                SelectionTitleBarView(
                    segments: contentTitlebarSegments,
                    showsBranch: false,
                    branchState: store.titlebarBranchState,
                    height: Theme.titlebarHeight,
                    titleYOffset: 0
                )
                .transition(.opacity)
            } else if store.sidebarCollapsed {
                Color.clear
                    .frame(height: Theme.titleStripHeight)
            }
            // Remote connection state (reconnecting / repair / offline)
            // surfaces as a banner in the same slot the local restart /
            // resume banners use.
            if let banner = contentConnectionBanner {
                RemoteHostConnectionBanner(banner: banner)
            }
            // Local scope: the bundled Host service did not answer. The
            // already-scanned disk view stays visible; this is the only
            // user-visible difference from a healthy launch.
            if !isRemoteScope {
                HostServiceStatusBanner()
            }
            // The session banners (restart recommendation, resume failure,
            // phone resize) render INSIDE their pane now — a split shows
            // them only on the affected pane, on its own surface color.
            ZStack {
                // Hidden, behind everything: panes for likely switch targets
                // attach + replay here before they are ever selected, so the
                // actual switch is instant. Sized like the real terminal
                // area so the replay renders at the right cols/rows.
                WarmPaneHostView(
                    sessions: prewarmSessions,
                    workingDirectories: prewarmWorkingDirectories,
                    cache: cache
                )
                if store.recentActivityVisible {
                    RecentActivityView(store: store)
                } else if let project = archivedProject {
                    ArchivedSessionsView(store: store, project: project)
                } else if let session = terminalSession {
                    if shouldMountPanePresentation(for: session) {
                        terminalMount(for: session)
                    } else {
                        DeadSessionView(
                            session: session,
                            canResume: store.sessionCanRestart(session.id),
                            isRestarting: store.restartingSessionIDs.contains(session.id),
                            onRestart: { store.restartSession(session.id) }
                        )
                    }
                } else if let project = launcherTargetProject {
                    SessionLauncherView(store: store, project: project)
                } else {
                    EmptyStateView()
                }
            }
            .frame(maxWidth: .infinity, maxHeight: .infinity)
            // Edge drop-zone highlight during an eligible session drag
            // (renders nothing when no zone is hovered).
            .overlay {
                TerminalPaneDropZonesOverlay(store: store)
            }
            .overlay {
                // With a translucent terminal the cover fades at the same
                // canvas alpha so a workspace switch never flashes opaque.
                Color(nsColor: selectedTerminalFrameBackground)
                    .opacity(workspaceTerminalFadeOpacity * transparency.surfaceOpacity)
                    .allowsHitTesting(false)
            }
            // Collapsed-sidebar card: the full-content pages get the same
            // slide-down the terminal panes do — the page becomes a rounded
            // card below the title strip instead of a full-bleed surface
            // running to the window top. Parameterized (never structural)
            // so toggling the sidebar can't remount the warm-pane host.
            .background {
                if collapsedSurfaceCard { SurfaceBackdrop() }
            }
            .clipShape(
                RoundedRectangle(
                    cornerRadius: collapsedSurfaceCard ? Theme.contentCornerRadius : 0,
                    style: .continuous
                )
            )
            .overlay {
                if collapsedSurfaceCard {
                    RoundedRectangle(
                        cornerRadius: Theme.contentCornerRadius,
                        style: .continuous
                    )
                    .strokeBorder(Theme.contentHairline, lineWidth: 1)
                    .allowsHitTesting(false)
                }
            }
        }
        // Live terminal: solid provider canvas under the whole column so any
        // residual edge (titlebar join, bottom of Metal surface, rounded
        // corner bleed) matches the TUI. settingsShellDim is semi-transparent
        // over vibrancy and reads as the wrong tint in those 1px gaps.
        // Translucent terminals drop this backstop: the Ghostty surface is
        // the only canvas paint, so gaps show the frame backdrop instead of
        // doubling the alpha.
        .background { columnBackdrop }
        // The collapsed-sidebar title strip fades in while the panes slide
        // down under it — same curve as the sidebar collapse itself.
        .animation(
            .timingCurve(0.25, 0.1, 0.25, 1, duration: 0.15),
            value: store.sidebarCollapsed
        )
    }

    /// Backdrop under the content column. Non-terminal pages (archived,
    /// recent, empty state, dead session) share the ONE Surface backdrop
    /// with the terminal and settings, so the main screen is a single
    /// background. Terminals — solo or split — sit on the window-frame
    /// material: every pane is its own rounded card carrying its own
    /// canvas, so everything around the cards (split gaps, the collapsed
    /// -sidebar title strip, corner bleed) shows the same backdrop that
    /// frames the content pane. Translucent Surfaces paint nothing here:
    /// the window-spanning frame backdrop already shows through, and a
    /// second material would double the wash.
    @ViewBuilder private var columnBackdrop: some View {
        if showsTerminal || collapsedSurfaceCard {
            if transparency.surfaceOpacity < 1 {
                Color.clear
            } else {
                FrameBackdrop()
            }
        } else {
            SurfaceBackdrop()
        }
    }

    /// Collapsed-sidebar presentation for the full-content pages (libraries,
    /// launcher, empty/dead states): the page renders as its own rounded
    /// card below the title strip — the frame material shows through around
    /// it, matching the terminal panes' slide-down.
    private var collapsedSurfaceCard: Bool {
        store.sidebarCollapsed && !showsTerminal
    }

    /// A main-pane library (archived sessions or All recent) is covering the
    /// terminal area.
    private var coversTerminal: Bool {
        archivedProject != nil || store.recentActivityVisible
    }

    private var showsTerminal: Bool {
        !coversTerminal
            && terminalSession.map(shouldMountPanePresentation(for:)) == true
    }

    private var selectedTerminalFrameBackground: NSColor {
        guard let session = terminalSession else {
            return Theme.terminalBackgroundNSColor
        }
        let representative = panePresentation(for: session).representative
        guard shouldMountTerminal(for: representative) else {
            return Theme.terminalBackgroundNSColor
        }
        return terminalFrameBackground(for: representative)
    }

    private var terminalSession: SessionEntry? {
        guard let id = selection.sessionID else { return nil }
        // Sessions filed in the project sidebar mount in the RIGHT panel — one
        // Ghostty surface cannot live in two containers, so the main area
        // never also mounts them (selecting one shows it on the right).
        if store.sessionIsInProjectSidebar(id) { return nil }
        return store.displaySessionsByID[id]
    }

    private var archivedProject: Project? {
        guard let id = store.archivedProjectID else { return nil }
        return store.displayProjectsByID[id]
    }

    /// Heading segments for the library pages' in-pane titlebar.
    private var contentTitlebarSegments: [String] {
        if store.recentActivityVisible {
            return ["Recent"]
        }
        if let project = archivedProject {
            let count = store.archivedSessions(projectID: project.id).count
            return [project.name, "Archived (\(count))"]
        }
        return store.titlebarSegments
    }

    /// The validated pane group currently shown, if any — drives the
    /// frame-material column backdrop behind the pane cards.
    private var shownPaneGroup: PaneGroup? {
        guard !coversTerminal, let session = terminalSession else { return nil }
        return store.validatedPaneGroup(containingSession: session.id)
    }

    /// Project whose session launcher fills the content area. Only shown when
    /// explicitly invoked (the Finder "New Unpeel Session Here" service sets
    /// `launcherProjectID`); otherwise the plain EmptyStateView shows, so a
    /// cold launch never lands on a project's launcher.
    private var launcherTargetProject: Project? {
        guard let id = store.launcherProjectID else { return nil }
        return store.displayProjectsByID[id]
    }

    private func terminalFrameBackground(for session: SessionEntry) -> NSColor {
        // Remote rows carry the Host-resolved canvas color; local styling
        // must never stat a Host-side working directory.
        if isRemoteScope {
            return store.remoteTerminalBackgroundColor(for: session.id)
                ?? Theme.terminalBackgroundNSColor
        }
        // Depend on themeRevision so OpenCode/Grok config + live canvas
        // samples repaint the titlebar / swap container without a switch.
        _ = cache.themeRevision
        return cache.frameStyle(
            for: session,
            workingDirectory: store.paneWorkingDirectory(for: session)
        ).backgroundColor
    }

    private func shouldMountTerminal(for session: SessionEntry) -> Bool {
        // A session being restarted stays selected (no empty-state flash), but
        // its host is being torn down — unmount the surface and show the
        // DeadSessionView/restart spinner until the replacement is live.
        session.status != .exited
            && !store.restartingSessionIDs.contains(session.id)
    }

    private func shouldMountPanePresentation(for session: SessionEntry) -> Bool {
        shouldMountTerminal(for: panePresentation(for: session).representative)
    }

    /// Pre-warm targets resolved to live sessions, excluding every Session
    /// surface already mounted by the pane container.
    private var prewarmSessions: [SessionEntry] {
        let shownSessionIDs = shownPaneSessionIDs
        return store.prewarmSessionIDs.compactMap { (id: String) -> SessionEntry? in
            guard !shownSessionIDs.contains(id),
                  // Pane-group members are actively mounted by the pane
                  // container — never also mount one as a warm pane (an NSView
                  // can have only one superview).
                  let session = store.sessionsByID[id], session.isAttachable,
                  // A warm pane mounts at the full terminal-area size, which
                  // would fight a phone-letterboxed session's grid.
                  store.phoneResizeOverrides[id] == nil
            else { return nil }
            return session
        }
    }

    /// Every Session surface mounted for the shown solo pane or valid group.
    private var shownPaneSessionIDs: Set<String> {
        guard let session = terminalSession else { return [] }
        return Set(panePresentation(for: session).entries.keys)
    }

    private struct PanePresentation {
        let representative: SessionEntry
        let group: PaneGroup?
        let entries: [String: SessionEntry]
    }

    /// Every scope uses the same pane container. Its transport switches behind
    /// each slot, while a solo Session is represented by a nil group. Keeping
    /// the outer identity on the representative Session means creating or
    /// closing a group never remounts that retained terminal surface.
    private func terminalMount(for session: SessionEntry) -> some View {
        let presentation = panePresentation(for: session)
        return TerminalPaneContainer(
            store: store,
            cache: cache,
            representative: presentation.representative,
            group: presentation.group,
            entries: presentation.entries,
            frameBackground: { terminalFrameBackground(for: $0) }
        )
        .id(
            "term:\(store.selectedHostScope.paneScopeID):"
                + presentation.representative.id
        )
        .transition(.identity)
        .animation(nil, value: presentation.group?.panes.map(\.id) ?? [])
    }

    /// Resolve a validated pane group into the Session entries its renderer
    /// needs. Launcher panes intentionally have no entry. If a projection
    /// changes between validation and lookup, fail closed to the shown solo
    /// Session for this render; the next store publication reconciles it.
    private func panePresentation(for session: SessionEntry) -> PanePresentation {
        let solo = PanePresentation(
            representative: session,
            group: nil,
            entries: [session.id: session]
        )
        guard let group = store.validatedPaneGroup(containingSession: session.id),
              let representativeID = group.representativeSessionID,
              let representative = store.displaySessionsByID[representativeID]
        else { return solo }

        var entries: [String: SessionEntry] = [:]
        for pane in group.panes {
            guard let sessionID = pane.content.sessionID else { continue }
            guard let entry = store.displaySessionsByID[sessionID] else {
                return solo
            }
            entries[sessionID] = entry
        }
        return PanePresentation(
            representative: representative,
            group: group,
            entries: entries
        )
    }

    /// RemoteHostRuntime needs the full visible set so every Controller-side
    /// pane keeps independent output, input, and resize ownership. The right
    /// project panel's members are presented terminals too — without them the
    /// scoped panel panes would render without an output stream.
    private var remotePresentedSessionIDs: Set<String> {
        guard isRemoteScope, !store.settingsVisible else { return [] }
        var ids = Set(store.projectSidebarSessions.compactMap { entry in
            shouldMountTerminal(for: entry) ? entry.id : nil
        })
        if !coversTerminal, let session = terminalSession {
            ids.formUnion(
                panePresentation(for: session).entries.values.compactMap { entry in
                    shouldMountTerminal(for: entry) ? entry.id : nil
                }
            )
        }
        return ids
    }

    private func syncRemotePresentedSessions() {
        store.remoteHostRuntime.setPresentedTerminalSessions(
            remotePresentedSessionIDs
        )
    }

    private var prewarmWorkingDirectories: [String: String] {
        var dirs: [String: String] = [:]
        for session in prewarmSessions {
            if let path = store.paneWorkingDirectory(for: session) {
                dirs[session.id] = path
            }
        }
        return dirs
    }
}

// MARK: - Session recommendation

struct RestartRecommendedBar: View {
    let recommendation: SessionRestartRecommendation
    /// The owning pane's terminal surface color — the bar reads as part of
    /// the pane, not a floating tinted strip.
    let background: Color
    let onRestart: () -> Void
    let onDismiss: () -> Void
    @State private var restartHovering = false
    @State private var dismissHovering = false

    var body: some View {
        HStack(spacing: 10) {
            Text(recommendation.action.map { "\($0.label) recommended" } ?? "Context queued")
                .font(.system(size: 12, weight: .semibold))
                .foregroundStyle(Theme.foreground)
                .lineLimit(1)
            Text(recommendation.message)
                .font(.system(size: 11))
                .foregroundStyle(Theme.mutedForeground)
                .lineLimit(1)
                .truncationMode(.tail)
            Spacer(minLength: 8)
            if let action = recommendation.action {
                Button(action: onRestart) {
                    HStack(spacing: 5) {
                        Image(systemName: "arrow.clockwise")
                            .font(.system(size: 10, weight: .semibold))
                        Text(action.label)
                            .font(.system(size: 11, weight: .semibold))
                    }
                    .foregroundStyle(Theme.foreground)
                    .padding(.horizontal, 9)
                    .frame(height: 24)
                    .background(
                        RoundedRectangle(cornerRadius: 6, style: .continuous)
                            .fill(Theme.foreground.opacity(restartHovering ? 0.13 : 0.08))
                    )
                    .overlay(
                        RoundedRectangle(cornerRadius: 6, style: .continuous)
                            .strokeBorder(Theme.foreground.opacity(0.10))
                    )
                }
                .buttonStyle(.plain)
                .onHover { restartHovering = $0 }
                .accessibilityLabel(action.label)
            }

            Button(action: onDismiss) {
                Image(systemName: "xmark")
                    .font(.system(size: 10, weight: .semibold))
                    .foregroundStyle(dismissHovering ? Theme.foreground : Theme.mutedForeground)
                    .frame(width: 24, height: 24)
                    .background(
                        RoundedRectangle(cornerRadius: 6, style: .continuous)
                            .fill(dismissHovering ? Theme.foreground.opacity(0.10) : .clear)
                    )
                    .contentShape(Rectangle())
            }
            .buttonStyle(.plain)
            .onHover { dismissHovering = $0 }
            .accessibilityLabel("Dismiss recommendation")
        }
        .padding(.horizontal, 12)
        .frame(height: 36)
        .background(background)
        .overlay(alignment: .bottom) {
            Rectangle()
                .fill(Theme.foreground.opacity(0.08))
                .frame(height: 1)
        }
    }
}

// MARK: - Resume failed banner

/// Shown when a restart-with-resume relaunch died because the provider's
/// conversation storage no longer exists on disk (UnpeelStore.resumeFailures)
/// — the CLI printed "no conversation found" and exited to a bare shell.
/// Same anatomy as RestartRecommendedBar; the action relaunches fresh.
struct ResumeFailedBar: View {
    /// See RestartRecommendedBar.background.
    let background: Color
    let onStartFresh: () -> Void
    let onDismiss: () -> Void
    @State private var freshHovering = false
    @State private var dismissHovering = false

    var body: some View {
        HStack(spacing: 10) {
            Text("Couldn't resume the conversation")
                .font(.system(size: 12, weight: .semibold))
                .foregroundStyle(Theme.foreground)
                .lineLimit(1)
            Text("Its history no longer exists on disk, so the agent can only start over.")
                .font(.system(size: 11))
                .foregroundStyle(Theme.mutedForeground)
                .lineLimit(1)
                .truncationMode(.tail)
            Spacer(minLength: 8)
            Button(action: onStartFresh) {
                HStack(spacing: 5) {
                    Image(systemName: "plus.circle")
                        .font(.system(size: 10, weight: .semibold))
                    Text("Start fresh")
                        .font(.system(size: 11, weight: .semibold))
                }
                .foregroundStyle(Theme.foreground)
                .padding(.horizontal, 9)
                .frame(height: 24)
                .background(
                    RoundedRectangle(cornerRadius: 6, style: .continuous)
                        .fill(Theme.foreground.opacity(freshHovering ? 0.13 : 0.08))
                )
                .overlay(
                    RoundedRectangle(cornerRadius: 6, style: .continuous)
                        .strokeBorder(Theme.foreground.opacity(0.10))
                )
            }
            .buttonStyle(.plain)
            .onHover { freshHovering = $0 }
            .accessibilityLabel("Start a fresh session")

            Button(action: onDismiss) {
                Image(systemName: "xmark")
                    .font(.system(size: 10, weight: .semibold))
                    .foregroundStyle(dismissHovering ? Theme.foreground : Theme.mutedForeground)
                    .frame(width: 24, height: 24)
                    .background(
                        RoundedRectangle(cornerRadius: 6, style: .continuous)
                            .fill(dismissHovering ? Theme.foreground.opacity(0.10) : .clear)
                    )
                    .contentShape(Rectangle())
            }
            .buttonStyle(.plain)
            .onHover { dismissHovering = $0 }
            .accessibilityLabel("Dismiss resume failure notice")
        }
        .padding(.horizontal, 12)
        .frame(height: 36)
        .background(background)
        .overlay(alignment: .bottom) {
            Rectangle()
                .fill(Theme.foreground.opacity(0.08))
                .frame(height: 1)
        }
    }
}

// MARK: - Warm pane host (hidden pre-attach mounting)

/// Mounts panes for pre-warm sessions into a hidden container that tracks
/// the terminal area's size. Being in the window (hidden is enough) is what
/// lets the Ghostty surface build and spawn its attach client; rendering
/// stays paused via `renderingPausedForPrewarm` until the pane is shown for
/// real. Never touches a pane that some other superview (the swap
/// container) currently owns.
struct WarmPaneHostView: NSViewRepresentable {
    let sessions: [SessionEntry]
    let workingDirectories: [String: String]
    let cache: SurfaceCache

    /// Sizes warm panes by hand instead of edge constraints, so tracking can
    /// pause during a live window resize: each hidden pane would otherwise
    /// run the full grid + PTY resize pipeline (socket roundtrip, host
    /// reflow, TUI repaint) on every drag frame — for panes nobody can see,
    /// multiplying the visible pane's cost by the prewarm count. One catch-up
    /// sync at didEndLiveResize preserves the "adoption needs no reflow"
    /// property.
    final class WarmPaneContainerView: NSView {
        override func layout() {
            super.layout()
            syncPaneFrames()
        }

        func syncPaneFrames(force: Bool = false) {
            if !force, window?.inLiveResize == true { return }
            for sub in subviews where sub.frame != bounds {
                sub.frame = bounds
            }
        }

        override func viewDidEndLiveResize() {
            super.viewDidEndLiveResize()
            syncPaneFrames(force: true)
        }
    }

    func makeNSView(context _: Context) -> WarmPaneContainerView {
        let view = WarmPaneContainerView()
        view.isHidden = true
        return view
    }

    func updateNSView(_ container: WarmPaneContainerView, context _: Context) {
        // Unmount warm panes that are no longer wanted (or were dropped from
        // the cache). Detached panes pause on their own (window == nil).
        for sub in container.subviews {
            guard let pane = sub as? GhosttyTerminalPane else { continue }
            let stillWanted = sessions.contains {
                cache.existingPane(for: $0.id) === pane
            }
            if !stillWanted {
                pane.removeFromSuperview()
            }
        }

        // Surface creation is synchronous main-thread work (process spawn +
        // Metal setup); cap it to ONE brand-new pane per update pass so a
        // hover sweep or ⌘-hold never stalls a single frame with several
        // creations. The remaining targets mount on a later pass (any store
        // publish re-runs this).
        var createdNewPane = false
        for session in sessions {
            let exists = cache.existingPane(for: session.id) != nil
            if !exists && createdNewPane { continue }

            let pane = cache.pane(
                for: session,
                workingDirectory: workingDirectories[session.id]
            )
            // Owned by the swap container (or anything else): hands off.
            if let superview = pane.superview, superview !== container { continue }
            guard pane.superview !== container else { continue }
            if !exists { createdNewPane = true }

            pane.translatesAutoresizingMaskIntoConstraints = true
            pane.autoresizingMask = []
            container.addSubview(pane)
        }
        container.syncPaneFrames()
    }
}

// MARK: - Surface host (retained swap container)

struct TerminalHostView: NSViewRepresentable {
    let session: SessionEntry
    let workingDirectory: String?
    /// `app-sessions` directory of the selected local scope (this home, or
    /// another local workspace's home). Threaded to the surface cache so
    /// the attach child streams the right `session.sock`.
    var sessionsDir: URL = LaunchConfig.appSessionsDir
    let frameBackgroundColor: NSColor
    /// From SurfaceCache — changes when OpenCode/Grok theme (config or live
    /// canvas sample) moves, so updateNSView re-paints even if the session
    /// id is unchanged.
    let themeRevision: UInt
    let phoneResize: PhoneResizeOverride?
    let cache: SurfaceCache
    /// Only the active slot may claim first responder when several retained
    /// terminal surfaces mount together.
    var isActive: Bool = true
    /// Called when a pointer press lands in this hosted terminal. A multi-pane
    /// view uses it to remember which independently mounted surface owns focus.
    var onActivate: (() -> Void)? = nil

    final class SwapContainer: NSView {
        var frameBackgroundColor = Theme.terminalBackgroundNSColor {
            didSet { updateLayer() }
        }
        var appliedThemeRevision: UInt = 0
        var representedSessionID: String?
        var pendingCreateSessionID: String?
        var isActive = false
        var onActivate: (() -> Void)?

        /// Phone-driven letterbox grid; nil = the pane fills the container.
        var phoneResize: PhoneResizeOverride? {
            didSet {
                guard phoneResize != oldValue else { return }
                scrollOffset = 0
                applyPaneLayout()
            }
        }

        private var laidOutPane: GhosttyTerminalPane?
        private var appliedLetterboxSize: CGSize?

        /// Vertical scroll position while a phone screen overflows the window
        /// (0 = bottom/prompt pinned; `letterboxOverflow` = top revealed).
        /// Adjusted by `scrollWheel`; reset when the override changes.
        private var scrollOffset: CGFloat = 0
        private var letterboxOverflow: CGFloat = 0

        /// Breathing room above and below the phone screen while letterboxed.
        private static let letterboxVerticalPadding: CGFloat = 20

        // Honest opacity: the container is only opaque while its backdrop
        // is (translucent terminals swap the fill for .clear).
        override var isOpaque: Bool { frameBackgroundColor.alphaComponent >= 1 }
        override var wantsUpdateLayer: Bool { true }

        override func hitTest(_ point: NSPoint) -> NSView? {
            let hit = super.hitTest(point)
            guard hit != nil,
                  let event = window?.currentEvent ?? NSApp.currentEvent,
                  event.type == .leftMouseDown
                    || event.type == .rightMouseDown
                    || event.type == .otherMouseDown
            else { return hit }
            onActivate?()
            return hit
        }

        /// Terminal background per appearance; AppKit re-runs updateLayer
        /// when the effective appearance flips, so the backdrop behind
        /// surface swaps always matches the Ghostty theme variant.
        override func updateLayer() {
            layer?.backgroundColor = frameBackgroundColor.cgColor
        }

        private var attachedPane: GhosttyTerminalPane? {
            subviews.lazy.compactMap { $0 as? GhosttyTerminalPane }.first
        }

        /// True when `pane` is a direct child here. The pane is ALWAYS a direct
        /// child — never reparented into a wrapper. A Ghostty Metal surface
        /// goes blank both inside an NSScrollView clip view and inside a nested
        /// clip NSView, so overflow/padding is done purely by framing the pane
        /// manually and clipping with the container's own `masksToBounds`.
        func hosts(_ pane: GhosttyTerminalPane) -> Bool {
            pane.superview === self
        }

        /// Removes any currently-hosted pane. The pane stays alive in the
        /// surface cache.
        func detachHostedPane() {
            for sub in subviews where sub is GhosttyTerminalPane {
                sub.removeFromSuperview()
            }
        }

        /// Vertical wheel scroll while a phone screen overflows the window.
        /// Best-effort: a full-screen TUI usually leaves vertical wheel events
        /// unconsumed, so they bubble up here; when it doesn't, the bottom
        /// stays pinned (the live prompt), which is the important part.
        override func scrollWheel(with event: NSEvent) {
            guard letterboxOverflow > 0 else {
                super.scrollWheel(with: event)
                return
            }
            let next = scrollOffset - event.scrollingDeltaY
            scrollOffset = min(max(next, 0), letterboxOverflow)
            layoutAttachedPane()
        }

        // The pane is framed manually, never with AutoLayout. A fixed-size
        // letterbox constraint exerts outward pressure on this container:
        // SwiftUI reads the fitting size as a layout minimum and the AppKit
        // engine can grow the container instead of clamping the pane (the
        // container is autoresizing-placed, which loses to a 999 size), so
        // shrinking the window below the phone grid shoved the whole content
        // column — titlebar and banner included — out of the window.
        override func setFrameSize(_ newSize: NSSize) {
            super.setFrameSize(newSize)
            layoutAttachedPane()
        }

        /// Frames the attached pane: full-bleed normally, or a fixed
        /// phone-shaped rectangle while a phone resize override is active. The
        /// pane is always a direct child; the letterbox keeps the screen's
        /// *full* grid height (aspect ratio forced, never shrunk to the
        /// window) and clips overflow to the container — the bottom (live
        /// prompt) stays pinned and `scrollWheel` reveals the top. The pane
        /// resize flows through the normal Ghostty→attach path, so the hosted
        /// PTY follows on its own.
        private func layoutAttachedPane() {
            guard let pane = attachedPane else { return }
            guard let target = phoneResize.flatMap({
                pane.letterboxSize(cols: $0.cols, rows: $0.rows)
            }) else {
                layer?.masksToBounds = false
                letterboxOverflow = 0
                pane.frame = bounds
                pane.setPhoneScreenFraming(false)
                return
            }
            // Reserve top+bottom padding out of the container height.
            let pad = min(Self.letterboxVerticalPadding, bounds.height / 4)
            let avail = max(0, bounds.height - pad * 2)

            // Width still clamps to the window (rare; windows are wide enough),
            // but height is forced to the full phone grid so the aspect holds.
            let width = min(ceil(target.width), bounds.width)
            let height = ceil(target.height)
            let overflow = max(0, height - avail)
            letterboxOverflow = overflow

            // Clip the overflow to the container instead of shrinking the grid.
            layer?.masksToBounds = true

            let x = ((bounds.width - width) / 2).rounded(.down)
            // Non-flipped coords: y is the pane's bottom edge. When it fits,
            // center with even gutters. When it overflows, pin the bottom a
            // `pad` above the container floor and let `scrollOffset` reveal the
            // top (0 = bottom/prompt visible, up to `overflow`).
            let y: CGFloat = overflow > 0
                ? pad + min(max(scrollOffset, 0), overflow)
                : ((bounds.height - height) / 2).rounded(.down)
            pane.frame = CGRect(x: x, y: y, width: width, height: height)
            pane.setPhoneScreenFraming(true)
        }

        /// Re-applies the pane layout after a state change (override set or
        /// cleared, grid metrics reported, pane attached).
        func applyPaneLayout() {
            guard let pane = attachedPane else { return }
            let target = phoneResize.flatMap {
                pane.letterboxSize(cols: $0.cols, rows: $0.rows)
            }
            // While letterboxed the pane's grid is phone-shaped and must not
            // become the launch-size estimate for new sessions. Keyed off the
            // override (not `target`): before cell metrics exist target is
            // still nil even though a letterbox is pending.
            pane.isLetterboxed = phoneResize != nil
            let changed = !(laidOutPane === pane && appliedLetterboxSize == target)
            laidOutPane = pane
            appliedLetterboxSize = target
            layoutAttachedPane()
            // Grid-metrics callbacks re-enter here after every sync; the
            // refit below only runs on a real change so a settled letterbox
            // stays settled (refit → metrics → refit would never converge).
            guard changed else { return }
            // A pane un-letterboxed while detached can carry grid drift the
            // debounced fit won't repair (same-size no-op); force the full
            // resize path once layout settles.
            DispatchQueue.main.async { [weak pane, weak self] in
                guard let pane, let self, self.hosts(pane) else { return }
                pane.refitNow()
            }
        }
    }

    func makeNSView(context _: Context) -> SwapContainer {
        let view = SwapContainer()
        view.wantsLayer = true
        return view
    }

    func updateNSView(_ container: SwapContainer, context _: Context) {
        // Translucent terminals: the Ghostty surface carries the only canvas
        // paint (see TerminalPaneStyle.backgroundOpacity) — an opaque swap
        // backdrop here would cancel the transparency.
        let fill = TransparencyModel.shared.surfaceOpacity < 1
            ? NSColor.clear
            : frameBackgroundColor
        // Always write the layer color — dynamic NSColor identity can stay
        // equal across resolution changes, and didSet would then no-op.
        container.frameBackgroundColor = fill
        container.layer?.backgroundColor = fill.cgColor
        container.representedSessionID = session.id
        let becameActive = isActive && !container.isActive
        container.isActive = isActive
        container.onActivate = onActivate
        container.phoneResize = phoneResize
        cache.noteShown(session.id)

        if let pane = cache.existingPane(for: session.id) {
            container.pendingCreateSessionID = nil
            // When the live theme revision moves, re-push Ghostty colors for
            // the already-attached pane (attach is a no-op if still hosted).
            if container.appliedThemeRevision != themeRevision {
                container.appliedThemeRevision = themeRevision
                let style = cache.frameStyle(
                    for: session,
                    workingDirectory: workingDirectory,
                    sessionsDir: sessionsDir
                )
                pane.applyPaneStyle(style.paneStyle)
            }
            let attached = attach(pane, to: container)
            if becameActive, !attached, container.hosts(pane) {
                pane.focus()
                pane.renderNow()
            }
            return
        }

        // Cold switch: paint the terminal background in this transaction,
        // then create the expensive Ghostty pane on the next main-loop turn.
        // Warm/existing panes still swap synchronously above.
        container.detachHostedPane()
        guard container.pendingCreateSessionID != session.id else { return }
        container.pendingCreateSessionID = session.id
        let session = session
        let workingDirectory = workingDirectory
        let sessionsDir = sessionsDir
        DispatchQueue.main.async { [cache, weak container] in
            guard let container,
                  container.representedSessionID == session.id,
                  container.pendingCreateSessionID == session.id
            else { return }
            container.pendingCreateSessionID = nil
            let pane = cache.pane(
                for: session,
                workingDirectory: workingDirectory,
                sessionsDir: sessionsDir
            )
            attach(pane, to: container)
        }
    }

    @discardableResult
    private func attach(
        _ pane: GhosttyTerminalPane,
        to container: SwapContainer
    ) -> Bool {
        guard !container.hosts(pane) else { return false }

        // Detach whatever pane is currently shown (it stays alive in the
        // cache — viewport state survives the swap).
        container.detachHostedPane()

        // addSubview re-parents an adopted warm pane automatically. The swap
        // container frames the pane manually (see SwapContainer); a pane
        // adopted from the warm host arrives with the flag off, and leaving
        // it off with no constraints would let the engine zero its frame.
        // applyPaneLayout re-parents it into the letterbox scroll host when a
        // phone resize is active.
        pane.translatesAutoresizingMaskIntoConstraints = true
        container.addSubview(pane)
        container.applyPaneLayout()
        // Cold-mounted panes report their first grid (and cell metrics)
        // only after initial layout; re-fit the letterbox once they exist.
        pane.onSurfaceGridChanged = { [weak pane, weak container] in
            guard let pane, let container, container.hosts(pane),
                  container.phoneResize != nil
            else { return }
            container.applyPaneLayout()
        }

        // Same-window re-parenting (warm host → swap container) fires no
        // viewDidMoveToWindow, so assert surface visibility explicitly —
        // without this an adopted pane can stay marked occluded while
        // displayed, and ghostty renders nothing for it (parses output,
        // never repaints; image-streaming app-mode sessions froze).
        pane.refreshSurfaceVisibility()

        // Focus and draw synchronously so the CATransaction committing this
        // swap already shows current content with a focused cursor. Without
        // this the first composited frame is the pane's stale pre-detach
        // drawable (the wrapper defers the post-attach draw one runloop
        // turn) and the cursor pops hollow→filled a frame later — together
        // they read as switch lag. window is nil on the very first mount
        // (makeNSView's view is not in the hierarchy yet during this pass);
        // the async fallback below covers that.
        if container.window != nil {
            if container.isActive {
                pane.focus()
            }
            pane.renderNow()
        }

        DispatchQueue.main.async { [weak pane, weak container] in
            // A rapid second switch may have already replaced this pane —
            // a stale focus would route keystrokes to a hidden session.
            guard let pane, let container, container.hosts(pane) else { return }
            // First-mount fallback: window was nil during the synchronous
            // pass above, so visibility could not be derived yet.
            pane.refreshSurfaceVisibility()
            if container.isActive {
                pane.focus()
            }
            // Layout has settled by now: force the full resize path so any
            // grid/layer drift the pane picked up while warm or detached is
            // repaired on every switch, instead of waiting for the user to
            // nudge the window (a same-size fit is a no-op to ghostty).
            pane.refitNow()
        }
        return true
    }
}

// MARK: - Scroll-to-bottom overlay

/// Visibility/action state for a pane's scroll-to-bottom button. Owned by
/// `GhosttyTerminalPane`; `visible` is flipped from Ghostty's scrollbar
/// metrics callback and the full-screen TUI jump-hint scan.
@MainActor
final class TerminalScrollButtonModel: ObservableObject {
    @Published var visible = false
    var action: () -> Void = {}
}

/// The button itself, mirroring the Tauri app's glass scroll-bottom button
/// (TerminalView.svelte): 36pt circle, chevron down, fades + slides in over
/// 0.2s, 0.88 opacity resting → 1 on hover. Native Liquid Glass on
/// macOS 26 (same `.glass` style as DeadSessionView's Restart button);
/// material-circle fallback on older systems. The 8pt padding gives the
/// slide-in headroom inside the hosting view.
struct TerminalScrollToBottomButton: View {
    @ObservedObject var model: TerminalScrollButtonModel
    @State private var hovering = false

    var body: some View {
        styledButton
            .onHover { hovering = $0 }
            .opacity(model.visible ? (hovering ? 1 : 0.88) : 0)
            .offset(y: model.visible ? 0 : 8)
            .allowsHitTesting(model.visible)
            .animation(.easeOut(duration: 0.2), value: model.visible)
            .padding(8)
            .accessibilityLabel("Scroll to bottom")
    }

    @ViewBuilder
    private var styledButton: some View {
        if #available(macOS 26.0, *) {
            Button(action: model.action) {
                chevron
                    // .glass pads the label to its own metrics; fix the
                    // label frame so the circle stays 36pt like the Tauri app.
                    .frame(width: 20, height: 20)
            }
            .buttonStyle(.glass)
            .buttonBorderShape(.circle)
            .controlSize(.large)
        } else {
            Button(action: model.action) {
                chevron
                    .frame(width: 36, height: 36)
                    .background(.ultraThinMaterial, in: Circle())
                    .overlay(Circle().strokeBorder(Theme.foreground.opacity(0.08)))
                    .contentShape(Circle())
            }
            .buttonStyle(.plain)
        }
    }

    private var chevron: some View {
        Image(systemName: "chevron.down")
            .font(.system(size: 13, weight: .semibold))
            .foregroundStyle(Theme.foreground.opacity(0.9))
    }
}

// MARK: - Empty / starting / dead states

struct EmptyStateView: View {
    var body: some View {
        VStack(spacing: 16) {
            PixelMascotView(size: 56)
            VStack(spacing: 8) {
                Text("No session selected")
                    .font(.system(size: 14))
                    .foregroundStyle(Theme.mutedForeground)
                Text("Pick a session in the sidebar, or hit + on a project")
                    .font(.system(size: 11))
                    .foregroundStyle(Theme.foreground.opacity(0.35))
            }
            Text(AppBrand.versionLabel)
                .font(.system(size: 10, weight: .medium))
                .foregroundStyle(Theme.foreground.opacity(0.25))
        }
        .frame(maxWidth: .infinity, maxHeight: .infinity)
    }
}

/// The Unpeel pixel mascot — the same 13×13 frames the iOS app bundles
/// (`MascotFrame0…3`, extracted losslessly from
/// `unpeel-mascot/mascot-animated.webp`), reused as the empty-state face.
/// Unlike the phone's idle loop, the desktop plays the animation ONCE on
/// appear (one pass through the 4 frames at the webp's 200ms cadence) and
/// then holds the resting frame — a greeting, not a distraction. Reduce
/// Motion (or a missing resource) skips straight to the static resting
/// frame; if no frame loads at all, the brand logo takes over. Decorative
/// only, so it is hidden from accessibility. `interpolation(.none)` keeps
/// the pixels crisp at any rendered size.
struct PixelMascotView: View {
    /// Rendered width in points (the frames are square).
    var size: CGFloat = 56

    @Environment(\.accessibilityReduceMotion) private var reduceMotion
    @State private var frameIndex = 0

    /// The webp's frame cadence: 4 frames × 200ms.
    private static let frameDuration: TimeInterval = 0.2

    /// The bundled animation frames, in playback order — or nil if the
    /// package resources are missing any of them (then AppBrandLogo draws).
    private static let frames: [NSImage]? = {
        let frames = (0..<4).compactMap { index -> NSImage? in
            guard let url = ModuleResources.url(
                forResource: "MascotFrame\(index)", withExtension: "png"
            ) else { return nil }
            return NSImage(contentsOf: url)
        }
        return frames.count == 4 ? frames : nil
    }()

    var body: some View {
        Group {
            if let frames = Self.frames {
                Image(nsImage: frames[min(frameIndex, frames.count - 1)])
                    .interpolation(.none)
                    .resizable()
                    .scaledToFit()
                    .task { await playLoop(frameCount: frames.count) }
            } else {
                AppBrandLogo()
                    .foregroundStyle(Theme.mutedForeground)
            }
        }
        .frame(width: size, height: size)
        .accessibilityHidden(true)
    }

    /// Loop the frames at the webp's cadence (like the phone). Cancels
    /// cleanly with the view via `.task`; static under Reduce Motion.
    private func playLoop(frameCount: Int) async {
        guard !reduceMotion else { return }
        let step = UInt64(Self.frameDuration * 1_000_000_000)
        while !Task.isCancelled {
            try? await Task.sleep(nanoseconds: step)
            guard !Task.isCancelled else { return }
            frameIndex = (frameIndex + 1) % frameCount
        }
    }
}

/// The Unpeel "U" mark (apps/website/components/Logo.tsx): two halves on a shared
/// viewBox, each its own template image so SwiftUI can tint both with the
/// surrounding foregroundStyle and fade just the top half. NSImage's SVG
/// decoder ignores per-path `fill-opacity`, so the fade has to live here.
struct AppBrandLogo: View {
    var body: some View {
        ZStack {
            half(AppBrand.logoBottom)
            half(AppBrand.logoTop).opacity(0.2)
        }
    }

    private func half(_ image: NSImage?) -> some View {
        Group {
            if let image {
                Image(nsImage: image)
                    .resizable()
                    .interpolation(.high)
                    .aspectRatio(contentMode: .fit)
            }
        }
    }
}

/// App branding for in-window chrome (the empty-state logo + version).
enum AppBrand {
    // L-normalized panel paths (NSImage's SVG decoder only handles M/L/C/Z).
    // LOGO:START — generated by scripts/update-logo.mjs from scripts/logo-source.svg
    /// Artwork coordinate space, e.g. "0 0 446 446".
    static let markViewBox = "0 0 446 446"
    /// Solid lower panel of the mark.
    static let logoBottomPath = "M408.833 446C429.36 446 446 429.36 446 408.833L446 223C446 212.737 437.68 204.417 427.417 204.417L364.233 204.417C353.97 204.417 345.65 212.737 345.65 223L345.65 327.067C345.65 337.33 337.33 345.65 327.067 345.65L118.933 345.65C108.67 345.65 100.35 337.33 100.35 327.067L100.35 223C100.35 212.737 92.03 204.417 81.7667 204.417L18.5833 204.417C8.32004 204.417 0 212.737 0 223L0 408.833C0 429.36 16.6401 446 37.1667 446L408.833 446Z"
    /// Upper panel of the mark.
    static let logoTopPath = "M1.62461e-05 37.1667C1.80406e-05 16.6401 16.6401 -1.19583e-06 37.1667 0L408.833 3.24921e-05C429.36 3.42866e-05 446 16.6401 446 37.1667L446 223L345.65 223L345.65 118.933C345.65 108.67 337.33 100.35 327.067 100.35L118.933 100.35C108.67 100.35 100.35 108.67 100.35 118.933L100.35 223L0 223L1.62461e-05 37.1667Z"
    // LOGO:END

    /// Builds a template NSImage from one or more panel paths. Template so the
    /// surrounding `foregroundStyle` (or the menu bar) tints it for light/dark.
    private static func mark(_ paths: String...) -> NSImage? {
        let body = paths.map { ##"<path d="\##($0)"/>"## }.joined()
        let svg = ##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="\##(markViewBox)" fill="#FFFFFF">\##(body)</svg>"##
        guard let image = NSImage(data: Data(svg.utf8)) else { return nil }
        image.isTemplate = true
        return image
    }

    /// Solid lower panel of the mark.
    static let logoBottom: NSImage? = mark(logoBottomPath)

    /// Faded upper panel of the mark.
    static let logoTop: NSImage? = mark(logoTopPath)

    /// White composite of the mark (solid lower + faded upper), non-template.
    /// The two panels carry different opacities — that two-tone is what makes
    /// the mark recognizable — but NSImage's SVG decoder drops per-path
    /// `fill-opacity`, so we composite them at their real opacities and bake
    /// that into the alpha. Base for both menu-bar variants below.
    private static func compositeMark(side: CGFloat = 16) -> NSImage? {
        func panel(_ d: String) -> NSImage? {
            NSImage(data: Data(##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="\##(markViewBox)" fill="#FFFFFF"><path d="\##(d)"/></svg>"##.utf8))
        }
        guard let lower = panel(logoBottomPath), let upper = panel(logoTopPath) else { return nil }
        return NSImage(size: NSSize(width: side, height: side), flipped: false) { rect in
            upper.draw(in: rect, from: .zero, operation: .sourceOver, fraction: 0.4)
            lower.draw(in: rect, from: .zero, operation: .sourceOver, fraction: 1.0)
            return true
        }
    }

    /// Template menu-bar mark — the menu bar tints it (respecting alpha) for
    /// light and dark automatically. Used for the idle state.
    static let menuBarMark: NSImage? = {
        let image = compositeMark()
        image?.isTemplate = true
        return image
    }()

    /// Colored, NON-template menu-bar image: the mark tinted to `foreground`
    /// (resolved by the caller from the menu bar's light/dark appearance) with a
    /// filled `badge` dot at the top-right. Used for the "done jobs" state — a
    /// template would flatten the badge to monochrome, so this keeps its color.
    static func menuBarBadgedMark(foreground: NSColor, badge: NSColor) -> NSImage? {
        guard let base = compositeMark() else { return nil }
        // A hair wider than tall so the badge can sit just past the mark's
        // top-right corner without clipping.
        let canvas = NSSize(width: 19, height: 16)
        let image = NSImage(size: canvas, flipped: false) { _ in
            let markRect = NSRect(x: 0, y: 0, width: 16, height: 16)
            base.draw(in: markRect)
            foreground.set()
            markRect.fill(using: .sourceAtop)        // tint the white mark
            // Badge dot at the top-right, just past the mark's corner.
            let d: CGFloat = 7
            let badgeRect = NSRect(x: canvas.width - d, y: canvas.height - d, width: d, height: d)
            badge.setFill()
            NSBezierPath(ovalIn: badgeRect).fill()
            return true
        }
        image.isTemplate = false
        return image
    }

    /// "vX.Y.Z" from the bundle's CFBundleShortVersionString; "dev" when run
    /// as a bare executable with no Info.plist.
    static let versionLabel: String = {
        let version = Bundle.main.object(forInfoDictionaryKey: "CFBundleShortVersionString") as? String
        guard let version, !version.isEmpty else { return "dev" }
        return "v\(version)"
    }()
}

struct StartingSessionView: View {
    let session: SessionEntry

    var body: some View {
        VStack(spacing: 10) {
            ToolIconView(command: session.presentationCommand, size: 32)
                .foregroundStyle(Theme.toolColor(forCommand: session.presentationCommand))
            Text(session.label)
                .font(.system(size: 14, weight: .medium))
                .foregroundStyle(Theme.foreground)
                .lineLimit(1)
            BrailleSpinner(color: Theme.toolSpinnerColor(forCommand: session.presentationCommand))
        }
        .frame(maxWidth: .infinity, maxHeight: .infinity)
    }
}

struct DeadSessionView: View {
    let session: SessionEntry
    /// Unknown non-empty commands have no trustworthy resume recipe. Keep
    /// their exited terminal/output visible without offering a button that
    /// would silently start unrelated fresh work.
    var canResume = true
    /// Restart in flight: the Svelte restart overlay swaps the button to a
    /// disabled in-flight state (App.svelte:1551-1554). Labelled "Resume":
    /// the session here is always stopped, and the relaunch continues its
    /// conversation.
    var isRestarting = false
    var onRestart: () -> Void = {}

    var body: some View {
        VStack(spacing: 10) {
            // The Svelte exited/restart overlays show the session's tool
            // icon at 32px (App.svelte:1521/1545, ToolIcon → commandIcon
            // in icons.ts, terminalIcon fallback for plain shells).
            ToolIconView(tool: QuickPresetTool.detect(in: session.command), size: 32)
                .foregroundStyle(Theme.toolColor(forCommand: session.command))
            Text(session.label)
                .font(.system(size: 14, weight: .medium))
                .foregroundStyle(Theme.foreground)
                .lineLimit(1)
            Text("Session exited")
                .font(.system(size: 11))
                .foregroundStyle(Theme.mutedForeground)
            // GlassButton under the label (App.svelte:1524-1528).
            // Same semantics as the Svelte app: the dead entry is removed
            // and a fresh session re-runs the original command in the
            // original cwd/worktree; the conversation itself is resumed
            // inside the CLI (e.g. `/resume`).
            if canResume {
                restartButton
                    .padding(.top, 2)
            }
        }
        .frame(maxWidth: .infinity, maxHeight: .infinity)
    }

    @ViewBuilder
    private var restartButton: some View {
        let button = Button(action: onRestart) {
            if isRestarting {
                HStack(spacing: 6) {
                    BrailleSpinner(color: Theme.toolSpinnerColor(forCommand: session.command))
                        .frame(width: 14, height: 14)
                    Text("Resuming")
                }
            } else {
                Text("Resume")
            }
        }
        .disabled(isRestarting)
        if #available(macOS 26.0, *) {
            button.buttonStyle(.glass)
        } else {
            button.buttonStyle(.bordered)
        }
    }
}
