//
//  ProjectSidebarView.swift
//  UnpeelNative
//
//  The right-side project panel: the terminal stack for sessions the user
//  filed into the current project's "Sidebar" group ("Move to Project
//  Sidebar" in the session context menu). No header — just the panes, in the
//  group's sidebar order, with draggable dividers between them (persisted
//  height weights). ⌘D while a panel member is selected appends a transient
//  launcher row that starts a new session directly in the group.
//
//  The group itself is ordinary shared state (a plain pinned child group), so
//  the left sidebar, the TUI, and the phone all see these sessions in that
//  top group; this view is only the desktop's presentation of its members.
//  RootView drives the panel's visibility off `store.projectSidebarSessions`.
//

import SwiftUI

struct ProjectSidebarView: View {
    @ObservedObject var store: UnpeelStore
    @ObservedObject var cache: SurfaceCache
    @ObservedObject private var transparency = TransparencyModel.shared

    /// Session-keyed relative height weights (missing = 1). Persisted so the
    /// stack's proportions survive restarts; live drag stays in this state
    /// and writes through on release.
    @State private var weights: [String: CGFloat] = ProjectSidebarView.loadWeights()
    /// Weights of the two panes flanking the divider at drag start.
    @State private var dividerDragBase: (above: CGFloat, below: CGFloat)?
    @State private var hoveredDividerIndex: Int?
    @State private var draggingDividerIndex: Int?

    private static let weightsKey = "unpeel.projectSidebar.weights"
    private static let minPaneHeight: CGFloat = 90
    private static let launcherHeight: CGFloat = 300

    var body: some View {
        let launcher = store.projectSidebarLauncher
        let pendingID = launcher?.pendingSessionID
        // While a launch is pending, its session renders INSIDE the launcher
        // slot (never as a second stack member), so the frame it lands in is
        // the frame the picker occupied — no jump at any step.
        let sessions = store.projectSidebarSessions.filter { $0.id != pendingID }
        GeometryReader { geo in
            let inset = Theme.surfaceInset
            let launcherProject = launcher == nil ? nil
                : store.currentRootProjectID.flatMap { store.displayProjectsByID[$0] }
            let launcherOpen = launcherProject != nil
            let slotCount = sessions.count + (launcherOpen ? 1 : 0)
            let gaps = CGFloat(max(0, slotCount - 1)) * inset
            // Collapsed left sidebar: the main panes slide down under the
            // title strip — the panel panes follow so their tops align.
            let topOffset: CGFloat = store.sidebarCollapsed ? Theme.titleStripHeight : 0
            let available = max(
                0, geo.size.height - inset * 2 - gaps - topOffset
            )
            // The launcher is an ordinary weight-1 slot: existing panes make
            // room the same way they would for the arriving pane itself.
            let extraWeight: CGFloat = launcherOpen ? 1 : 0
            let heights = resolvedHeights(
                sessions: sessions, available: available, extraWeight: extraWeight
            )
            let launcherHeight = available
                * 1 / max(totalWeight(sessions) + extraWeight, 0.001)

            // The launcher renders WHERE the new pane will arrive — directly
            // below its anchor member (or at the end without one), matching
            // the main area's launcher-pane-at-destination behavior.
            let anchorID = launcher?.afterSessionID
            let launcherIndex: Int? = !launcherOpen ? nil
                : (anchorID.flatMap { id in
                    sessions.firstIndex(where: { $0.id == id }).map { $0 + 1 }
                } ?? sessions.count)

            VStack(spacing: 0) {
                ForEach(Array(sessions.enumerated()), id: \.element.id) { index, session in
                    if index > 0 {
                        divider(index: index, sessions: sessions, available: available)
                    }
                    pane(for: session)
                        .frame(height: heights[session.id] ?? 0)
                    if let project = launcherProject, launcherIndex == index + 1 {
                        launcherRow(
                            project: project,
                            height: launcherHeight,
                            pendingID: pendingID
                        )
                        .padding(.top, inset)
                    }
                }
                if let project = launcherProject, launcherIndex == 0 {
                    launcherRow(
                        project: project,
                        height: launcherHeight,
                        pendingID: pendingID
                    )
                    .padding(.top, sessions.isEmpty ? 0 : inset)
                }
            }
            .padding(.top, inset + topOffset)
            .padding(.bottom, inset)
            // Breathing room on BOTH sides: leading clears the divider
            // hairline, trailing matches the window frame gap.
            .padding(.leading, inset)
            .padding(.trailing, inset)
        }
        .animation(.easeInOut(duration: 0.15), value: store.sidebarCollapsed)
        .onChange(of: store.projectSidebarSessions.map(\.id)) { ids in
            pruneWeights(to: ids)
            // The pending session landed: hand the launcher slot's frame over
            // to its pane (a pure bookkeeping swap — same index, same height).
            store.clearProjectSidebarLauncherIfLanded()
        }
    }

    /// The transient "add a pane" launcher, styled as a TEMPORARY PANE — the
    /// same header-strip + terminal-canvas presentation the main area's
    /// launcher pane uses, so the picker reads as the pane it will become.
    /// After a preset is chosen it keeps its frame and shows the starting
    /// spinner until the session lands in this exact slot.
    private func launcherRow(
        project: Project, height: CGFloat, pendingID: String?
    ) -> some View {
        let background = launcherCanvasColor
        let starting = pendingID != nil
        let shape = RoundedRectangle(
            cornerRadius: Theme.contentCornerRadius, style: .continuous
        )
        return VStack(spacing: 0) {
            HStack(spacing: 5) {
                Text(starting ? "Starting…" : "New pane")
                    .font(Theme.sessionLabelFont)
                    .foregroundStyle(Theme.mutedForeground)
                    .lineLimit(1)
                    .padding(.horizontal, 7)
                Spacer(minLength: 0)
                if !starting {
                    Button {
                        store.projectSidebarLauncher = nil
                    } label: {
                        Image(systemName: "xmark")
                            .font(.system(size: 10, weight: .semibold))
                            .foregroundStyle(Theme.mutedForeground)
                            .frame(width: 22, height: 22)
                            .contentShape(Rectangle())
                    }
                    .buttonStyle(.plain)
                    .help("Cancel")
                }
            }
            .padding(.horizontal, 8)
            .padding(.top, 3.5)
            .padding(.bottom, 2)
            .frame(height: Theme.sessionRowHeight + 5.5)
            .background(
                Color(nsColor: background)
                    .opacity(transparency.surfaceOpacity)
            )

            ZStack {
                Color(nsColor: background)
                    .opacity(transparency.surfaceOpacity)
                if starting {
                    BrailleSpinner(color: Theme.mutedForeground)
                } else {
                    SessionLauncherView(
                        store: store,
                        project: project,
                        compact: true,
                        onLaunch: { store.launchInProjectSidebar(preset: $0) },
                        onCancel: { store.projectSidebarLauncher = nil }
                    )
                }
            }
            .frame(maxHeight: .infinity)
        }
        .frame(height: height)
        .frame(maxWidth: .infinity)
        .clipShape(shape)
        .overlay(shape.strokeBorder(Theme.contentHairline, lineWidth: 1))
    }

    /// Canvas behind the launcher pane: the anchor member's frame color so
    /// the temporary pane matches the stack it joins.
    private var launcherCanvasColor: NSColor {
        let anchorID = store.projectSidebarLauncher?.afterSessionID
        if let anchor = store.projectSidebarSessions.first(where: { $0.id == anchorID })
            ?? store.projectSidebarSessions.first {
            return frameBackground(for: anchor)
        }
        return Theme.terminalBackgroundNSColor
    }

    // MARK: Heights

    private func totalWeight(_ sessions: [SessionEntry]) -> CGFloat {
        sessions.reduce(0) { $0 + (weights[$1.id] ?? 1) }
    }

    private func resolvedHeights(
        sessions: [SessionEntry], available: CGFloat, extraWeight: CGFloat = 0
    ) -> [String: CGFloat] {
        guard !sessions.isEmpty else { return [:] }
        let sum = max(totalWeight(sessions) + extraWeight, 0.001)
        var heights: [String: CGFloat] = [:]
        for session in sessions {
            heights[session.id] = available * (weights[session.id] ?? 1) / sum
        }
        return heights
    }

    /// Divider between panes `index-1` and `index`: the surface-inset gap is
    /// the hit area (same affordance as the split dividers), dragging
    /// transfers height between the two flanking panes.
    private func divider(
        index: Int, sessions: [SessionEntry], available: CGFloat
    ) -> some View {
        ZStack {
            if hoveredDividerIndex == index || draggingDividerIndex == index {
                Capsule()
                    .fill(Theme.paneDividerLineHover)
                    .frame(width: 36, height: 3)
                    .transition(.opacity)
            }
        }
        // Size the hit area BEFORE contentShape/gesture: an empty ZStack has
        // zero intrinsic height, so a frame applied after them would leave
        // nothing to grab.
        .frame(maxWidth: .infinity)
        .frame(height: Theme.surfaceInset)
        .contentShape(Rectangle())
        .animation(.easeInOut(duration: 0.12), value: hoveredDividerIndex)
        .gesture(
            // Global coordinate space: the divider MOVES during the drag, so
            // a local-space translation oscillates against its own feedback
            // (visible as fighting/blinking).
            DragGesture(minimumDistance: 1, coordinateSpace: .global)
                .onChanged { value in
                    let aboveID = sessions[index - 1].id
                    let belowID = sessions[index].id
                    if dividerDragBase == nil {
                        dividerDragBase = (
                            weights[aboveID] ?? 1, weights[belowID] ?? 1
                        )
                        draggingDividerIndex = index
                    }
                    guard let base = dividerDragBase, available > 0 else { return }
                    // The pair's combined weight is invariant during the
                    // drag, so the total stays stable for px→weight mapping.
                    let sum = max(totalWeight(sessions), 0.001)
                    let deltaW = value.translation.height / available * sum
                    let minW = Self.minPaneHeight / available * sum
                    var above = base.above + deltaW
                    var below = base.below - deltaW
                    if above < minW {
                        below -= (minW - above)
                        above = minW
                    }
                    if below < minW {
                        above -= (minW - below)
                        below = minW
                    }
                    weights[aboveID] = above
                    weights[belowID] = below
                }
                .onEnded { _ in
                    dividerDragBase = nil
                    draggingDividerIndex = nil
                    Self.saveWeights(weights)
                }
        )
        .onHover { inside in
            // A drag in progress owns the cursor; the divider slides under
            // the pointer as heights change, and reacting to those enter/exit
            // edges would flicker the cursor and the grip.
            guard draggingDividerIndex == nil else { return }
            hoveredDividerIndex = inside ? index : nil
            if inside {
                NSCursor.resizeUpDown.push()
            } else {
                NSCursor.pop()
            }
        }
    }

    // MARK: Panes

    /// One stacked member: the same solo pane presentation the main area uses
    /// (pane header chip + terminal), never part of a split group.
    private func pane(for session: SessionEntry) -> some View {
        TerminalPaneContainer(
            store: store,
            cache: cache,
            representative: session,
            group: nil,
            entries: [session.id: session],
            frameBackground: { frameBackground(for: $0) },
            isAuxiliaryRegion: true
        )
        // Distinct mount identity from the main area (which excludes panel
        // members via `sessionIsInProjectSidebar`, so a surface never mounts
        // in two containers).
        .id("project-sidebar:" + session.id)
    }

    /// Canvas color per scope, matching `ContentArea.terminalFrameBackground`:
    /// scoped rows carry the Host-resolved color; local styling never stats a
    /// scoped working directory.
    private func frameBackground(for session: SessionEntry) -> NSColor {
        if store.selectedHostScope != .local {
            return store.remoteTerminalBackgroundColor(for: session.id)
                ?? Theme.terminalBackgroundNSColor
        }
        _ = cache.themeRevision
        return cache.frameStyle(
            for: session,
            workingDirectory: store.paneWorkingDirectory(for: session)
        ).backgroundColor
    }

    // MARK: Weight persistence

    private func pruneWeights(to ids: [String]) {
        let keep = Set(ids)
        let pruned = weights.filter { keep.contains($0.key) }
        if pruned != weights {
            weights = pruned
            Self.saveWeights(pruned)
        }
    }

    private static func loadWeights() -> [String: CGFloat] {
        let raw = AppDefaults.shared.dictionary(forKey: weightsKey) ?? [:]
        return raw.compactMapValues { value in
            (value as? Double).map { CGFloat(max($0, 0.05)) }
        }
    }

    private static func saveWeights(_ weights: [String: CGFloat]) {
        if weights.isEmpty {
            AppDefaults.shared.removeObject(forKey: weightsKey)
        } else {
            AppDefaults.shared.set(
                weights.mapValues { Double($0) }, forKey: weightsKey
            )
        }
    }
}
