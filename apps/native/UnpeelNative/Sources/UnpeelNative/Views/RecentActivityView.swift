//
//  RecentActivityView.swift
//  UnpeelNative
//
//  The app-wide "All recent" page: live active sessions on top, then the
//  persisted activity feed (ActivityLog.swift) grouped by day. Opened from
//  the "All recent" footer link in the titlebar/menu-bar activity dropdowns
//  (or ⇧⌘R); rendered in the main content pane with the exact shell and
//  card language of ArchivedSessionsView.
//

import SwiftUI

struct RecentActivityView: View {
    @ObservedObject var store: UnpeelStore

    var body: some View {
        VStack(spacing: 0) {
            if store.activeJobSessions.isEmpty && feedEntries.isEmpty {
                emptyState
            } else {
                list
            }
        }
        .frame(maxWidth: .infinity, maxHeight: .infinity)
        .onExitCommand {
            store.closeRecentActivity()
        }
    }

    // MARK: - List

    private var list: some View {
        ScrollView {
            LazyVStack(alignment: .leading, spacing: 1) {
                activeSection
                feedSections
            }
            .padding(.horizontal, 30)
            .padding(.top, 12)
            .padding(.bottom, 30)
            .frame(maxWidth: 820)
            .frame(maxWidth: .infinity)
        }
    }

    private var activeSection: some View {
        Group {
            let jobs = store.activeJobSessions
            if !jobs.isEmpty {
                sectionLabel("Active")
                ForEach(jobs) { session in
                    RecentActivityRow(
                        title: cardTitle(session.label),
                        project: store.activityProjectName(session.projectID),
                        event: store.activityStatusLabel(for: session),
                        command: session.presentationCommand,
                        working: true,
                        unread: false
                    ) {
                        store.closeRecentActivity()
                        store.revealSessionInSidebar(session.id)
                    }
                }
            }
        }
    }

    /// Feed entries newest-first, minus sessions that are actively working
    /// right now (they're already in Active above).
    private var feedEntries: [ActivityLogEntry] {
        let active = Set(store.activeJobSessions.map(\.id))
        return store.activityLogEntries.reversed().filter { !active.contains($0.sessionID) }
    }

    private var feedSections: some View {
        Group {
            ForEach(Self.groupByDay(feedEntries), id: \.label) { group in
                sectionLabel(group.label)
                ForEach(group.entries) { entry in
                    feedRow(entry)
                }
            }
        }
    }

    private func feedRow(_ entry: ActivityLogEntry) -> some View {
        // Prefer the live title (auto-title/rename overlay applied) while the
        // session still exists; the logged snapshot is the fallback that keeps
        // removed sessions renderable.
        let session = store.sessionsByID[entry.sessionID]
        let sessionAlive = session != nil
        return RecentActivityRow(
            title: cardTitle(session?.label ?? entry.title),
            project: store.projectsByID[entry.projectID] != nil
                ? store.activityProjectName(entry.projectID) : entry.projectName,
            event: Self.kindLabel(
                entry.kind,
                message: entry.message,
                age: Self.ageString(entry.date)
            ),
            command: entry.command,
            working: false,
            unread: store.unreadSessionIDs.contains(entry.sessionID)
        ) {
            guard sessionAlive else { return }
            store.closeRecentActivity()
            store.revealSessionInSidebar(entry.sessionID)
        }
        .opacity(sessionAlive ? 1 : 0.55)
    }

    private var emptyState: some View {
        VStack(spacing: 10) {
            Image(systemName: "bell")
                .font(.system(size: 30, weight: .medium))
                .foregroundStyle(Theme.mutedForeground.opacity(0.7))
            Text("No recent activity")
                .font(.system(size: 14, weight: .medium))
                .foregroundStyle(Theme.foreground)
            Text("Session starts, finishes, input requests, and App alerts will appear here.")
                .font(.system(size: 11))
                .foregroundStyle(Theme.mutedForeground)
        }
        .frame(maxWidth: .infinity, maxHeight: .infinity)
        .padding(.bottom, 52)
    }

    private func sectionLabel(_ text: String) -> some View {
        Text(text)
            .font(.system(size: 11, weight: .semibold))
            .foregroundStyle(Theme.mutedForeground)
            .padding(.horizontal, 10)
            .padding(.top, 12)
            .padding(.bottom, 3)
    }

    private func cardTitle(_ raw: String) -> String {
        let title = raw.trimmingCharacters(in: .whitespacesAndNewlines)
        return title.isEmpty ? "Untitled session" : title
    }

    // MARK: - Grouping / formatting

    struct DayGroup {
        let label: String
        let entries: [ActivityLogEntry]
    }

    static func groupByDay(_ entries: [ActivityLogEntry], now: Date = Date()) -> [DayGroup] {
        let calendar = Calendar.current
        var order: [Date] = []
        var byDay: [Date: [ActivityLogEntry]] = [:]
        for entry in entries {
            let day = calendar.startOfDay(for: entry.date)
            if byDay[day] == nil { order.append(day) }
            byDay[day, default: []].append(entry)
        }
        let formatter = DateFormatter()
        formatter.setLocalizedDateFormatFromTemplate("MMMd")
        return order.map { day in
            let label: String
            if calendar.isDate(day, inSameDayAs: now) {
                label = "Today"
            } else if let yesterday = calendar.date(byAdding: .day, value: -1, to: now),
                      calendar.isDate(day, inSameDayAs: yesterday) {
                label = "Yesterday"
            } else {
                label = formatter.string(from: day)
            }
            return DayGroup(label: label, entries: byDay[day] ?? [])
        }
    }

    static func kindLabel(
        _ kind: ActivityLogEntry.Kind,
        message: String? = nil,
        age: String
    ) -> String {
        let when = age == "now" ? "just now" : "\(age) ago"
        switch kind {
        case .started: return "Started \(when)"
        case .needsInput: return "Needed input \(when)"
        case .finished: return "Finished \(when)"
        case .exited: return "Exited \(when)"
        case .alert:
            return message.map { "\($0) · \(when)" } ?? "Alerted \(when)"
        }
    }

    /// "now / 5m / 3h / 2d" — same scale as SessionEntry.ageString.
    static func ageString(_ date: Date, now: Date = Date()) -> String {
        let seconds = max(0, now.timeIntervalSince(date))
        if seconds < 60 { return "now" }
        let minutes = Int(seconds / 60)
        if minutes < 60 { return "\(minutes)m" }
        let hours = minutes / 60
        if hours < 24 { return "\(hours)h" }
        return "\(hours / 24)d"
    }
}

// MARK: - Row

/// Compact single-line table row (matches ArchivedSessionCard's row form):
/// bare CLI logo, title, then muted trailing columns — project and event —
/// with a subtle hover highlight. Working rows swap the logo for the
/// CLI-colored spinner; unread rows carry the blue dot.
private struct RecentActivityRow: View {
    let title: String
    let project: String
    let event: String
    let command: String
    let working: Bool
    let unread: Bool
    let onSelect: () -> Void

    @State private var hovering = false

    var body: some View {
        Button(action: onSelect) {
            HStack(spacing: 10) {
                Group {
                    if working {
                        BrailleSpinner(color: Theme.toolSpinnerColor(forCommand: command))
                    } else {
                        ToolIconView(command: command, size: 15)
                            .foregroundStyle(Theme.toolColor(forCommand: command))
                    }
                }
                .frame(width: 16, height: 16)

                Text(title)
                    .font(.system(size: 13))
                    .foregroundStyle(Theme.foreground)
                    .lineLimit(1)
                    .truncationMode(.tail)

                Spacer(minLength: 12)

                if !project.isEmpty {
                    Text(project)
                        .font(.system(size: 11))
                        .foregroundStyle(Theme.mutedForeground)
                        .lineLimit(1)
                }
                Text(event)
                    .font(.system(size: 11))
                    .foregroundStyle(Theme.mutedForeground)
                    .lineLimit(1)
                    .frame(minWidth: 120, alignment: .trailing)

                if unread {
                    Circle()
                        .fill(Theme.unread)
                        .frame(width: 7, height: 7)
                }
            }
            .padding(.horizontal, 10)
            .padding(.vertical, 6)
            .frame(maxWidth: .infinity, alignment: .leading)
            .background(
                RoundedRectangle(cornerRadius: 7, style: .continuous)
                    .fill(hovering ? Theme.hoverRow : .clear)
            )
            .contentShape(Rectangle())
        }
        .buttonStyle(.plain)
        .compatFocusEffectDisabled()
        .onHover { hovering = $0 }
    }
}
