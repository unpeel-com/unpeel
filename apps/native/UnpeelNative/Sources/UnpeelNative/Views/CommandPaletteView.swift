//
//  CommandPaletteView.swift
//  UnpeelNative
//
//  ⌘K command palette + ⌃Tab MRU session switcher overlays (RootView).
//
//  The palette is the keyboard "jump to anything": sessions across all
//  projects, projects themselves, preset launches, and a few app commands.
//  It is deliberately NOT a code-tool palette (no files, no symbols — see
//  AGENTS.md Product Philosophy); everything it lists is a session-level
//  noun or verb that already exists elsewhere in the UI.
//
//  Both overlays own their transient state locally; the store only hosts
//  `commandPaletteVisible` / `sessionSwitcher` so the menu bar, key
//  monitors, and views agree on visibility.
//

import AppKit
import SwiftUI

// MARK: - Fuzzy matching

/// Small case-insensitive matcher: exact substring beats subsequence,
/// earlier and word-start matches beat scattered ones. nil = no match.
enum PaletteFuzzy {
    static func score(_ query: String, in candidate: String) -> Int? {
        if query.isEmpty { return 0 }
        let loweredQuery = query.lowercased()
        let loweredCandidate = candidate.lowercased()
        if let range = loweredCandidate.range(of: loweredQuery) {
            let position = loweredCandidate.distance(
                from: loweredCandidate.startIndex, to: range.lowerBound
            )
            return 1000 - min(position, 500)
        }
        let queryChars = Array(loweredQuery)
        let candidateChars = Array(loweredCandidate)
        var queryIndex = 0
        var score = 0
        var lastMatch = -2
        for (index, char) in candidateChars.enumerated() {
            guard queryIndex < queryChars.count else { break }
            guard char == queryChars[queryIndex] else { continue }
            score += lastMatch == index - 1 ? 15 : 5
            let isWordStart = index == 0
                || " -_/.".contains(candidateChars[index - 1])
            if isWordStart { score += 10 }
            lastMatch = index
            queryIndex += 1
        }
        return queryIndex == queryChars.count ? score : nil
    }
}

// MARK: - Shared card glass

/// The overlay cards' Liquid Glass shell: real `.glassEffect` on macOS 26+,
/// frosted material + hairline fallback below.
private struct OverlayCardStyle: ViewModifier {
    var cornerRadius: CGFloat = 16

    func body(content: Content) -> some View {
        if #available(macOS 26.0, *) {
            content
                .glassEffect(
                    .regular,
                    in: RoundedRectangle(cornerRadius: cornerRadius, style: .continuous)
                )
                .shadow(color: .black.opacity(0.3), radius: 30, y: 12)
        } else {
            content
                .background(
                    ZStack {
                        VisualEffectBackground(
                            material: .popover, blendingMode: .withinWindow
                        )
                        Theme.contentTint
                    }
                    .clipShape(
                        RoundedRectangle(cornerRadius: cornerRadius, style: .continuous)
                    )
                    .shadow(color: .black.opacity(0.35), radius: 30, y: 12)
                )
                .overlay(
                    RoundedRectangle(cornerRadius: cornerRadius, style: .continuous)
                        .strokeBorder(Theme.foreground.opacity(0.08))
                )
        }
    }
}

extension View {
    fileprivate func overlayCardGlass(cornerRadius: CGFloat = 16) -> some View {
        modifier(OverlayCardStyle(cornerRadius: cornerRadius))
    }
}

// MARK: - Palette items

struct PaletteItem: Identifiable {
    enum Kind: String {
        case session = "Session"
        case project = "Project"
        case launch = "Launch"
        case command = "Command"
    }

    /// Leading-slot indicator, mirroring the sidebar rows exactly: spinner
    /// while working, attention dot while blocked, nothing otherwise.
    enum Indicator {
        case spinner(Color)
        case attention
    }

    let id: String
    let kind: Kind
    let title: String
    let subtitle: String?
    /// Extra text the fuzzy matcher may hit (e.g. the session's command).
    let keywords: String
    var indicator: Indicator?
    /// Sidebar-parity unread marker (blue dot after the title).
    var unread = false
    /// When set, the leading slot shows the tool's brand mark instead of
    /// the spinner/attention indicator (used for `Launch` rows).
    var iconCommand: String?
    /// Dim section caption drawn above this row (empty-query tiers only).
    var header: String?
    /// Optional trailing metadata that replaces the generic item kind.
    /// Session rows use their lifecycle age, matching date-sorted sidebar rows.
    var trailingLabel: String?
    let action: @MainActor () -> Void
}

// MARK: - ⌘K palette overlay

struct CommandPaletteOverlay: View {
    @ObservedObject var store: UnpeelStore

    @State private var query = ""
    @State private var selectedIndex = 0
    @State private var keyMonitor: Any?
    /// Shared-recency snapshot (UnpeelStore.sessionRecencyMs), taken once
    /// when the palette opens: ordering must not stat session dirs on every
    /// keystroke, and seeding it in init keeps the first frame in final
    /// order (no reorder flash after onAppear).
    @State private var recencyByID: [String: Int64]

    init(store: UnpeelStore) {
        self.store = store
        _recencyByID = State(initialValue: store.paletteRecencySnapshot())
    }
    /// Last mouse position (screen coords) that drove a hover-selection.
    /// Rows appearing or scrolling under a stationary cursor re-fire hover
    /// without the mouse moving — selection must stay keyboard-owned then.
    @State private var lastHoverMouseLocation: NSPoint?
    @FocusState private var searchFocused: Bool

    private static let maxResults = 40

    var body: some View {
        GeometryReader { geo in
            ZStack(alignment: .top) {
                // Click-away scrim. Kept light: the palette should feel like
                // a layer over the workspace, not a modal takeover.
                Color.black.opacity(0.22)
                    .ignoresSafeArea()
                    .contentShape(Rectangle())
                    .onTapGesture { close() }

                // Adaptive to small windows: width yields before 560, the
                // top inset compresses, and the list height (set in
                // `palette`) is bounded by what actually fits.
                palette(listMaxHeight: max(120, geo.size.height * 0.55))
                    .frame(maxWidth: min(560, max(280, geo.size.width - 32)))
                    .padding(.top, min(120, max(20, geo.size.height * 0.14)))
                    .frame(maxWidth: .infinity)
            }
        }
        .onAppear {
            installKeyMonitor()
            // Seed with the cursor's current position so the row that
            // happens to sit under it when the palette opens doesn't steal
            // the selection from the top item.
            lastHoverMouseLocation = NSEvent.mouseLocation
            // Focus lands after the field mounts.
            DispatchQueue.main.async { searchFocused = true }
        }
        .onDisappear { removeKeyMonitor() }
    }

    private func palette(listMaxHeight: CGFloat) -> some View {
        VStack(spacing: 0) {
            TextField("Search sessions, projects, commands…", text: $query)
                .textFieldStyle(.plain)
                .font(.system(size: 16))
                .focused($searchFocused)
                .padding(.horizontal, 16)
                .padding(.vertical, 13)
                .onChange(of: query) { _ in selectedIndex = 0 }
                // Enter in the field is a fallback for the key monitor.
                .onSubmit { runSelected() }

            Divider()

            if results.isEmpty {
                Text("No matches")
                    .font(.system(size: 12))
                    .foregroundStyle(Theme.mutedForeground)
                    .frame(maxWidth: .infinity)
                    .padding(.vertical, 22)
            } else {
                ScrollViewReader { proxy in
                    ScrollView {
                        VStack(spacing: 1) {
                            ForEach(Array(results.enumerated()), id: \.element.id) { index, item in
                                PaletteRowView(
                                    item: item,
                                    isSelected: index == selectedIndex
                                )
                                .id(item.id)
                                .onTapGesture { run(item) }
                                .onHover { hovering in
                                    guard hovering else { return }
                                    let location = NSEvent.mouseLocation
                                    guard location != lastHoverMouseLocation
                                    else { return }
                                    lastHoverMouseLocation = location
                                    selectedIndex = index
                                }
                            }
                        }
                        .padding(6)
                    }
                    .frame(maxHeight: min(360, listMaxHeight))
                    .onChange(of: selectedIndex) { index in
                        guard results.indices.contains(index) else { return }
                        proxy.scrollTo(results[index].id)
                    }
                }
            }
        }
        .overlayCardGlass()
    }

    // MARK: Items

    /// A session row plus the facts the tiered empty-query view groups by.
    private struct SessionMeta {
        let item: PaletteItem
        let working: Bool
        let unread: Bool
        /// The top-level sidebar project this session lives under (its own
        /// project for root sessions, the parent for worktree/group rows) —
        /// "current project" for the palette means this family.
        let topLevelProjectID: String
    }

    /// Sessions across every project/worktree in "All recent" order —
    /// the shared ⌘K contract with the TUI (unpeel-core
    /// session_ops::recents_recency_ms): working sessions first, then
    /// everything by recency (newest lifecycle event, creation as floor) — with
    /// the sidebar's exact indicator language (spinner while working,
    /// attention dot, trailing unread dot — nothing otherwise).
    private var sessionMeta: [SessionMeta] {
        var byID: [String: SessionMeta] = [:]
        var order: [(id: String, working: Bool, recency: Int64)] = []
        func walk(_ node: ProjectNode, topLevelProjectID: String) {
            for session in node.sessions
            where !store.archivedSessionIDs.contains(session.id) {
                let recency = recencyByID[session.id]
                    ?? max(session.createdAt, session.lifecycleAtMs ?? 0)
                let indicator: PaletteItem.Indicator? = switch session.status {
                case .starting, .busy:
                    .spinner(Theme.toolSpinnerColor(forCommand: session.presentationCommand))
                case .attention:
                    .attention
                case .idle, .exited:
                    nil
                }
                let working = session.status == .starting || session.status == .busy
                    || store.restartingSessionIDs.contains(session.id)
                byID[session.id] = SessionMeta(
                    item: PaletteItem(
                        id: "session:\(session.id)",
                        kind: .session,
                        title: session.label,
                        subtitle: node.project.name,
                        keywords: session.command,
                        indicator: indicator,
                        unread: store.unreadSessionIDs.contains(session.id),
                        trailingLabel: session.ageString(since: recency),
                        action: { [id = session.id] in
                            store.revealSessionInSidebar(id)
                        }
                    ),
                    working: working,
                    unread: store.unreadSessionIDs.contains(session.id),
                    topLevelProjectID: topLevelProjectID
                )
                order.append((
                    id: session.id,
                    working: working,
                    recency: recency
                ))
            }
            node.worktrees.forEach { walk($0, topLevelProjectID: topLevelProjectID) }
        }
        store.displayNodes.forEach { walk($0, topLevelProjectID: $0.id) }
        order.sort { a, b in
            if a.working != b.working { return a.working }
            if a.recency != b.recency { return a.recency > b.recency }
            return a.id < b.id
        }
        var metas: [SessionMeta] = []
        var seen = Set<String>()
        for entry in order where seen.insert(entry.id).inserted {
            if let meta = byID[entry.id] { metas.append(meta) }
        }
        return metas
    }

    private var sessionItems: [PaletteItem] { sessionMeta.map(\.item) }

    /// The top-level project family the empty-query palette stays close to:
    /// the selected session's, else the ⌘N launch target's.
    private var currentTopLevelProjectID: String? {
        func holdsSession(_ node: ProjectNode, _ id: String) -> Bool {
            node.sessions.contains { $0.id == id }
                || node.worktrees.contains { holdsSession($0, id) }
        }
        func holdsProject(_ node: ProjectNode, _ id: String) -> Bool {
            node.id == id || node.worktrees.contains { holdsProject($0, id) }
        }
        if let selected = store.selectedSession?.id,
            let top = store.displayNodes.first(where: { holdsSession($0, selected) }) {
            return top.id
        }
        if let target = store.defaultLaunchProjectID,
            let top = store.displayNodes.first(where: { holdsProject($0, target) }) {
            return top.id
        }
        return nil
    }

    /// Archived sessions across every project — surfaced in ⌘K when the
    /// query matches them, so the palette also searches the archive without
    /// opening the per-project library. Excluded from the empty-query recents.
    private var archivedSessionItems: [PaletteItem] {
        var items: [PaletteItem] = []
        func walk(_ node: ProjectNode) {
            for session in store.archivedSessions(in: node) {
                let subtitle = "Archived · \(node.project.name)"
                let keywords = [session.command, session.worktreeBranch]
                    .compactMap { $0 }
                    .joined(separator: " ")
                items.append(PaletteItem(
                    id: "archived:\(session.id)",
                    kind: .session,
                    title: session.label,
                    subtitle: subtitle,
                    keywords: keywords,
                    action: { [id = session.id] in
                        // Restore to sidebar and reveal — same as the
                        // archive library's Restore action.
                        store.restoreArchivedSessionToSidebar(id)
                    }
                ))
            }
            node.worktrees.forEach(walk)
        }
        store.displayNodes.forEach(walk)
        return items
    }

    /// Empty-query session cap: recents, not an inventory.
    private static let recentSessionLimit = 5

    /// Row after the recents that opens the app-wide All recent page.
    private var allSessionsLink: PaletteItem {
        PaletteItem(
            id: "command:all-sessions",
            kind: .command,
            title: "All sessions",
            subtitle: "⇧⌘R",
            keywords: "recent activity history",
            action: { store.openRecentActivity() }
        )
    }

    /// Projects (incl. worktree children): focus = expand + select the
    /// most recent session, or open the launcher when empty.
    private var projectItems: [PaletteItem] {
        var items: [PaletteItem] = []
        func walkProjects(_ node: ProjectNode) {
            if node.project.isFolder != true {
                items.append(PaletteItem(
                    id: "project:\(node.id)",
                    kind: .project,
                    title: node.project.name,
                    subtitle: node.project.worktreeBranch,
                    keywords: node.project.path,
                    action: { [id = node.id] in store.focusProject(id) }
                ))
            }
            node.worktrees.forEach(walkProjects)
        }
        store.displayNodes.forEach(walkProjects)
        return items
    }

    /// Preset launches and app commands, in base order.
    private var actionItems: [PaletteItem] {
        var items: [PaletteItem] = []

        // Preset launches in the current (⌘N-target) project.
        if let projectID = store.defaultLaunchProjectID {
            let projectName = store.displayProjectsByID[projectID]?.name ?? ""
            for preset in [Preset.newTerminal] + store.availablePresets {
                items.append(PaletteItem(
                    id: "preset:\(preset.id)",
                    kind: .launch,
                    title: "New session: \(preset.label)",
                    subtitle: "in \(projectName)",
                    keywords: preset.command,
                    iconCommand: preset.command,
                    action: { [preset] in
                        store.launchSession(
                            projectID: projectID,
                            command: preset.command,
                            sourcePresetID: preset.command.isEmpty ? nil : preset.id
                        )
                    }
                ))
            }
        }

        // App commands (each already has a home elsewhere in the UI).
        items.append(PaletteItem(
            id: "command:new-terminal", kind: .command,
            title: "New Terminal", subtitle: "⌘T", keywords: "shell blank",
            action: { store.launchDefaultTerminal() }
        ))
        items.append(PaletteItem(
            id: "command:toggle-sidebar", kind: .command,
            title: "Toggle Sidebar", subtitle: "⌘B", keywords: "hide show",
            action: { store.sidebarCollapsed.toggle() }
        ))
        items.append(PaletteItem(
            id: "command:settings", kind: .command,
            title: "Settings", subtitle: "⌘,", keywords: "preferences",
            action: { store.openSettings(tab: nil) }
        ))
        return items
    }

    /// The unfiltered palette, tiered (mirrored with the TUI's
    /// `palette_sections`): every working session (any project — a job you
    /// kicked off elsewhere still matters), every unread-finished session
    /// (the popover's blue-dot group), then the current project's remaining
    /// recents so keyboard nav stays close to home. Idle sessions in other
    /// projects only surface by typing; the Projects tier switches project
    /// instead. Captions ride on the first row of each tier.
    private var tieredRecents: [PaletteItem] {
        let metas = sessionMeta
        let current = currentTopLevelProjectID
        var items: [PaletteItem] = []
        func tier(_ header: String?, _ rows: [PaletteItem]) {
            guard var first = rows.first else { return }
            first.header = header
            items.append(first)
            items += rows.dropFirst()
        }
        tier("Active", metas.filter(\.working).map(\.item))
        tier("Recent", metas.filter { !$0.working && $0.unread }.map(\.item))
        let home = metas.filter {
            !$0.working && !$0.unread
                && (current == nil || $0.topLevelProjectID == current)
        }
        tier(
            current.flatMap { store.displayProjectsByID[$0]?.name },
            Array(home.prefix(Self.recentSessionLimit).map(\.item))
        )
        items.append(allSessionsLink)
        tier("Projects", projectItems.filter { $0.id != "project:\(current ?? "")" })
        tier(nil, actionItems)
        return Array(items.prefix(Self.maxResults))
    }

    private var results: [PaletteItem] {
        let trimmed = query.trimmingCharacters(in: .whitespaces)
        guard !trimmed.isEmpty else { return tieredRecents }
        let sessions = sessionItems
        let archived = archivedSessionItems
        let regularResults = (sessions + [allSessionsLink] + projectItems + actionItems)
            .compactMap { item -> (PaletteItem, Int)? in
                let titleScore = PaletteFuzzy.score(trimmed, in: item.title)
                    .map { $0 * 2 }
                let subtitleScore = item.subtitle
                    .flatMap { PaletteFuzzy.score(trimmed, in: $0) }
                let keywordScore = PaletteFuzzy.score(trimmed, in: item.keywords)
                let best = [titleScore, subtitleScore, keywordScore]
                    .compactMap { $0 }
                    .max()
                return best.map { (item, $0) }
            }
            .sorted { $0.1 > $1.1 }
            .prefix(Self.maxResults)
            .map(\.0)
        // Archived matches append after regular results so active sessions
        // rank first, but the palette still finds archived sessions when
        // nothing active matches (the "search archive" case).
        if regularResults.isEmpty {
            let archivedResults = archived
                .compactMap { item -> (PaletteItem, Int)? in
                    let titleScore = PaletteFuzzy.score(trimmed, in: item.title).map { $0 * 2 }
                    let subtitleScore = item.subtitle.flatMap { PaletteFuzzy.score(trimmed, in: $0) }
                    let keywordScore = PaletteFuzzy.score(trimmed, in: item.keywords)
                    guard let best = [titleScore, subtitleScore, keywordScore].compactMap({ $0 }).max() else { return nil }
                    return (item, best)
                }
                .sorted { $0.1 > $1.1 }
                .prefix(Self.maxResults)
                .map(\.0)
            if !archivedResults.isEmpty { return Array(archivedResults) }
        } else if !archived.isEmpty {
            // When there are regular hits, still surface top archived hits
            // below them (up to 5) so a narrow query that hits both doesn't
            // hide the archive.
            let archivedResults = archived
                .compactMap { item -> (PaletteItem, Int)? in
                    let titleScore = PaletteFuzzy.score(trimmed, in: item.title).map { $0 * 2 }
                    let subtitleScore = item.subtitle.flatMap { PaletteFuzzy.score(trimmed, in: $0) }
                    let keywordScore = PaletteFuzzy.score(trimmed, in: item.keywords)
                    guard let best = [titleScore, subtitleScore, keywordScore].compactMap({ $0 }).max() else { return nil }
                    return (item, best)
                }
                .sorted { $0.1 > $1.1 }
                .prefix(5)
                .map(\.0)
            if !archivedResults.isEmpty {
                return Array((regularResults + archivedResults).prefix(Self.maxResults))
            }
        }
        return Array(regularResults)
    }

    // MARK: Keyboard

    private func installKeyMonitor() {
        guard keyMonitor == nil else { return }
        keyMonitor = NSEvent.addLocalMonitorForEvents(
            matching: .keyDown
        ) { event in
            let consumed = MainActor.assumeIsolated { () -> Bool in
                switch event.keyCode {
                case 126: // up
                    moveSelection(-1)
                    return true
                case 125: // down
                    moveSelection(1)
                    return true
                case 36, 76: // return / keypad enter
                    runSelected()
                    return true
                case 53: // esc
                    close()
                    return true
                default:
                    return false
                }
            }
            return consumed ? nil : event
        }
    }

    private func removeKeyMonitor() {
        if let keyMonitor { NSEvent.removeMonitor(keyMonitor) }
        keyMonitor = nil
    }

    private func moveSelection(_ delta: Int) {
        let count = results.count
        guard count > 0 else { return }
        selectedIndex = (selectedIndex + delta + count) % count
    }

    private func runSelected() {
        let items = results
        guard items.indices.contains(selectedIndex) else { return }
        run(items[selectedIndex])
    }

    private func run(_ item: PaletteItem) {
        close()
        item.action()
    }

    private func close() {
        store.commandPaletteVisible = false
    }
}

private struct PaletteRowView: View {
    let item: PaletteItem
    let isSelected: Bool

    var body: some View {
        VStack(alignment: .leading, spacing: 2) {
            if let header = item.header {
                Text(header)
                    .font(.system(size: 10, weight: .medium))
                    .foregroundStyle(Theme.mutedForeground)
                    .opacity(0.8)
                    .padding(.horizontal, 10)
                    .padding(.top, 7)
            }
            row
        }
    }

    private var row: some View {
        HStack(spacing: 8) {
            // Fixed leading slot keeps titles aligned. For preset launches
            // (`New session: …`) we show the tool's brand mark; for
            // sessions we keep the sidebar-parity spinner/attention slot.
            ZStack {
                if let command = item.iconCommand {
                    ToolIconView(command: command, size: 14)
                        .foregroundStyle(Theme.toolColor(forCommand: command))
                } else {
                    switch item.indicator {
                    case .spinner(let color):
                        BrailleSpinner(color: color)
                    case .attention:
                        AttentionDot(color: Theme.attention)
                    case nil:
                        EmptyView()
                    }
                }
            }
            .frame(width: 16, height: 16)

            Text(item.title)
                .font(.system(size: 13))
                .foregroundStyle(Theme.foreground)
                .lineLimit(1)
                .truncationMode(.tail)

            // Sidebar-parity unread marker (7px #60a5fa after the title).
            if item.unread {
                Circle()
                    .fill(Theme.unread)
                    .frame(width: 7, height: 7)
            }

            if let subtitle = item.subtitle, item.trailingLabel == nil {
                Text(subtitle)
                    .font(.system(size: 11))
                    .foregroundStyle(Theme.mutedForeground)
                    .lineLimit(1)
                    .truncationMode(.middle)
            }

            Spacer(minLength: 8)

            Text(item.trailingLabel ?? item.kind.rawValue)
                .font(.system(size: 9, weight: .medium))
                .foregroundStyle(Theme.mutedForeground)
                .opacity(0.7)

            if let subtitle = item.subtitle, item.trailingLabel != nil {
                Text(subtitle)
                    .font(.system(size: 11))
                    .foregroundStyle(Theme.mutedForeground)
                    .lineLimit(1)
                    .truncationMode(.middle)
            }
        }
        .padding(.horizontal, 10)
        .padding(.vertical, 6)
        .background(
            RoundedRectangle(cornerRadius: 7, style: .continuous)
                .fill(isSelected ? Theme.activeRow : .clear)
        )
        .contentShape(Rectangle())
    }
}

// MARK: - ⌃Tab session switcher overlay

struct SessionSwitcherOverlay: View {
    @ObservedObject var store: UnpeelStore

    var body: some View {
        if let state = store.sessionSwitcher {
            VStack(spacing: 1) {
                ForEach(Array(state.sessionIDs.enumerated()), id: \.element) { index, id in
                    if let session = store.displaySessionsByID[id] {
                        switcherRow(
                            session: session,
                            projectName: store.displayProjectsByID[session.projectID]?.name ?? "",
                            isSelected: index == state.index
                        )
                    }
                }

                Text("⌃Tab cycle · release ⌃ to switch · esc cancel")
                    .font(.system(size: 10))
                    .foregroundStyle(Theme.mutedForeground)
                    .opacity(0.8)
                    .padding(.top, 7)
                    .padding(.bottom, 3)
            }
            .padding(8)
            .frame(maxWidth: 420)
            .overlayCardGlass()
            .padding(.horizontal, 16)
            .frame(maxWidth: .infinity, maxHeight: .infinity)
        }
    }

    private func switcherRow(
        session: SessionEntry, projectName: String, isSelected: Bool
    ) -> some View {
        HStack(spacing: 8) {
            Circle()
                .fill(statusColor(session.status))
                .frame(width: 7, height: 7)

            Text(session.label)
                .font(.system(size: 13))
                .foregroundStyle(Theme.foreground)
                .lineLimit(1)
                .truncationMode(.tail)

            Spacer(minLength: 8)

            Text(projectName)
                .font(.system(size: 11))
                .foregroundStyle(Theme.mutedForeground)
                .lineLimit(1)
                .truncationMode(.middle)
        }
        .padding(.horizontal, 10)
        .padding(.vertical, 6)
        .background(
            RoundedRectangle(cornerRadius: 7, style: .continuous)
                .fill(isSelected ? Theme.activeRow : .clear)
        )
    }

    private func statusColor(_ status: SessionStatus) -> Color {
        switch status {
        case .attention: return Theme.attention
        case .busy, .starting: return Theme.accent
        case .idle: return Theme.mutedForeground
        case .exited: return Theme.mutedForeground.opacity(0.4)
        }
    }
}
