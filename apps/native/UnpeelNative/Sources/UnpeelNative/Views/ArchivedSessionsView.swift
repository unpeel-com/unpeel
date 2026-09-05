//
//  ArchivedSessionsView.swift
//  UnpeelNative
//
//  Project-scoped archived session library. A non-zero Cleanup preview
//  setting can keep recent archived sessions in the sidebar; right-clicking
//  a project opens the complete main-pane library.
//

import SwiftUI

struct ArchivedSessionsView: View {
    @ObservedObject var store: UnpeelStore
    let project: Project

    @State private var query = ""

    private var sessions: [SessionEntry] {
        store.archivedSessions(projectID: project.id)
    }

    private var filteredSessions: [SessionEntry] {
        let q = query.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !q.isEmpty else { return sessions }
        return sessions
            .compactMap { session -> (SessionEntry, Int)? in
                let titleScore = PaletteFuzzy.score(q, in: session.label).map { $0 * 2 }
                let commandScore = PaletteFuzzy.score(q, in: session.command)
                let branchScore = session.worktreeBranch.flatMap { PaletteFuzzy.score(q, in: $0) }
                guard let best = [titleScore, commandScore, branchScore].compactMap({ $0 }).max() else { return nil }
                return (session, best)
            }
            .sorted { $0.1 > $1.1 }
            .map(\.0)
    }

    var body: some View {
        // No in-pane header: the title bar reads "<project> / Archived (N)"
        // (contentTitlebarSegments in TerminalArea), and Esc closes.
        VStack(spacing: 0) {
            if !sessions.isEmpty {
                searchField
            }
            Group {
                if sessions.isEmpty {
                    emptyState
                } else if filteredSessions.isEmpty {
                    noResultsState
                } else {
                    sessionList
                }
            }
            .frame(maxWidth: .infinity, maxHeight: .infinity)
        }
        .onExitCommand {
            store.closeArchivedSessions()
        }
    }

    private var searchField: some View {
        HStack(spacing: 6) {
            Image(systemName: "magnifyingglass")
                .font(.system(size: 11, weight: .medium))
                .foregroundStyle(Theme.mutedForeground)
            TextField("Search archived sessions", text: $query)
                .textFieldStyle(.plain)
                .font(.system(size: 12))
            if !query.isEmpty {
                Button {
                    query = ""
                } label: {
                    Image(systemName: "xmark.circle.fill")
                        .font(.system(size: 11))
                        .foregroundStyle(Theme.mutedForeground.opacity(0.7))
                }
                .buttonStyle(.plain)
            }
        }
        .padding(.horizontal, 9)
        .padding(.vertical, 6)
        .background(
            RoundedRectangle(cornerRadius: 7, style: .continuous)
                .fill(Theme.activeRow.opacity(0.7))
        )
        .overlay(
            RoundedRectangle(cornerRadius: 7, style: .continuous)
                .stroke(Theme.resizerLine, lineWidth: 1)
        )
        .padding(.horizontal, 30)
        .padding(.top, 12)
        .padding(.bottom, 8)
        .frame(maxWidth: 820)
        .frame(maxWidth: .infinity)
    }

    private var sessionList: some View {
        ScrollView {
            LazyVStack(spacing: 1) {
                ForEach(filteredSessions) { session in
                    ArchivedSessionCard(store: store, session: session)
                }
            }
            .padding(.horizontal, 30)
            .padding(.top, 6)
            .padding(.bottom, 30)
            .frame(maxWidth: 820)
            .frame(maxWidth: .infinity)
        }
    }

    private var emptyState: some View {
        VStack(spacing: 10) {
            Image(systemName: "archivebox")
                .font(.system(size: 30, weight: .medium))
                .foregroundStyle(Theme.mutedForeground.opacity(0.7))
            Text("No archived sessions")
                .font(.system(size: 14, weight: .medium))
                .foregroundStyle(Theme.foreground)
            Text("Sessions you archive from \(project.name) will appear here.")
                .font(.system(size: 11))
                .foregroundStyle(Theme.mutedForeground)
        }
        .frame(maxWidth: .infinity, maxHeight: .infinity)
        .padding(.bottom, 52)
    }

    private var noResultsState: some View {
        VStack(spacing: 8) {
            Image(systemName: "magnifyingglass")
                .font(.system(size: 22, weight: .medium))
                .foregroundStyle(Theme.mutedForeground.opacity(0.7))
            Text("No matches")
                .font(.system(size: 13, weight: .medium))
                .foregroundStyle(Theme.foreground)
            Text("No archived sessions match \"\(query)\".")
                .font(.system(size: 11))
                .foregroundStyle(Theme.mutedForeground)
                .multilineTextAlignment(.center)
                .lineLimit(2)
        }
        .frame(maxWidth: .infinity, maxHeight: .infinity)
        .padding(.bottom, 52)
        .padding(.horizontal, 30)
    }
}

private struct ArchivedSessionCard: View {
    @ObservedObject var store: UnpeelStore
    let session: SessionEntry

    @State private var hovering = false

    private var isStopping: Bool {
        session.isLive
    }

    private var isRemoving: Bool {
        store.removingSessionIDs.contains(session.id)
    }

    private var isRestarting: Bool {
        store.restartingSessionIDs.contains(session.id)
    }

    private var isBusy: Bool {
        isStopping || isRemoving || isRestarting
    }

    private var canResume: Bool {
        !isBusy && store.sessionCanRestart(session.id)
    }

    private var isConfirmingRemove: Bool {
        store.confirmingRemoveSessionID == session.id
            && store.confirmingRemoveSurface == .archivePage
    }

    var body: some View {
        HStack(spacing: 10) {
            toolMark

            Text(session.label)
                .font(.system(size: 13))
                .foregroundStyle(Theme.foreground)
                .lineLimit(1)
                .truncationMode(.tail)

            Spacer(minLength: 12)

            if isConfirmingRemove {
                removeConfirmation
            } else {
                Text(subtitleText)
                    .font(.system(size: 11))
                    .foregroundStyle(Theme.mutedForeground)
                    .lineLimit(1)
                actions
            }
        }
        .padding(.horizontal, 10)
        .padding(.vertical, 5)
        .background(
            RoundedRectangle(cornerRadius: 7, style: .continuous)
                .fill(hovering ? Theme.hoverRow : .clear)
        )
        .contentShape(Rectangle())
        .onHover { hovering = $0 }
        .contextMenu {
            archiveMenuItems
        }
        .background {
            // Click-away/Esc dismissal scoped to THIS row (sidebar parity):
            // a click anywhere outside the card cancels the pending confirm.
            if isConfirmingRemove {
                RemoveConfirmDismissMonitor(onCancel: {
                    store.cancelRemoveConfirm()
                })
            }
        }
    }

    private var toolMark: some View {
        Group {
            if isStopping {
                BrailleSpinner(color: Theme.toolSpinnerColor(forCommand: session.command))
            } else {
                ToolIconView(command: session.command, size: 15)
                    .foregroundStyle(Theme.toolColor(forCommand: session.command))
            }
        }
        .frame(width: 16, height: 16)
    }

    private var subtitleText: String {
        if isRemoving { return "Removing · \(startedText)" }
        if isRestarting { return "Resuming · \(startedText)" }
        if isStopping { return "Stopping terminal… · \(startedText)" }
        return startedText
    }

    private var startedText: String {
        let age = session.ageString()
        return age == "now" ? "Started just now" : "Started \(age) ago"
    }

    @ViewBuilder
    private var actions: some View {
        HStack(spacing: 8) {
            // The row's inline status text already covers the busy states, so
            // busy rows just show no action button.
            if canResume {
                EditorButton(title: "Restore & Resume", variant: .primary, size: .small) {
                    store.resumeArchivedSession(session.id)
                }
            } else if !isBusy {
                EditorButton(title: "Restore", size: .small) {
                    store.restoreArchivedSessionToSidebar(session.id)
                }
            }

            Menu {
                archiveMenuItems
            } label: {
                Image(systemName: "ellipsis")
                    .font(.system(size: 13, weight: .semibold))
                    .foregroundStyle(Theme.mutedForeground)
                    .frame(width: 28, height: 24)
                    .contentShape(Rectangle())
            }
            .menuStyle(.borderlessButton)
            .menuIndicator(.hidden)
            .fixedSize()
            .help("Archived session actions")
        }
    }

    private var removeConfirmation: some View {
        HStack(spacing: 8) {
            Text("Remove from Unpeel?")
                .font(.system(size: 11, weight: .medium))
                .foregroundStyle(Theme.foreground)
            EditorButton(title: "Cancel", size: .small) {
                store.cancelRemoveConfirm()
            }
            EditorButton(title: "Delete", variant: .danger, size: .small) {
                store.confirmRemoveSession(session.id)
            }
        }
    }

    @ViewBuilder
    private var archiveMenuItems: some View {
        if canResume {
            Button("Restore & Resume") {
                store.resumeArchivedSession(session.id)
            }
        }
        Button("Restore to sidebar") {
            store.restoreArchivedSessionToSidebar(session.id)
        }
        .disabled(isBusy)

        if session.supportsTranscriptCopy {
            Menu("Copy transcript") {
                Button("Last 20 entries") {
                    store.copyTranscriptMarkdown(session.id, entries: 20)
                }
                Button("Last 50 entries") {
                    store.copyTranscriptMarkdown(session.id, entries: 50)
                }
                Button("Whole conversation") {
                    store.copyTranscriptMarkdown(session.id, entries: 0)
                }
            }
        }

        Divider()

        Button("Delete permanently", role: .destructive) {
            store.requestRemoveSession(session.id, from: .archivePage)
        }
        .disabled(isBusy)
    }
}
