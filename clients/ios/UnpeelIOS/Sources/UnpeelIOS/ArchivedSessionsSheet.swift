//
//  ArchivedSessionsSheet.swift
//  UnpeelIOS
//
//  A project's archive library, opened from the project's organize sheet —
//  the phone twin of the desktop's archived-sessions pane. Rows are fetched
//  from GET /mobile/archive (Mac-resolved summaries, newest first) and offer
//  Restore (back into the regular sidebar) or Restore & Resume
//  (restore + the desktop restart path, which continues the conversation).
//  Presented at the ROOT like every top-bar sheet — a `.sheet` nested over
//  the Metal terminal surface doesn't present reliably.
//

import SwiftUI
import UnpeelShared

struct ArchivedSessionsSheet: View {
    var store: RemotePreviewStore
    let project: RemoteProjectSummary

    @State private var sessions: [RemoteSessionSummary]?
    @State private var loadError: String?
    /// Session id with a restore/resume in flight (disables its buttons).
    @State private var busySessionID: String?

    var body: some View {
        NavigationStack {
            Group {
                if let sessions {
                    if sessions.isEmpty {
                        emptyState
                    } else {
                        list(sessions)
                    }
                } else if let loadError {
                    errorState(loadError)
                } else {
                    ProgressView()
                        .frame(maxWidth: .infinity, maxHeight: .infinity)
                }
            }
            .navigationTitle("Archive — \(project.name)")
            .navigationBarTitleDisplayMode(.inline)
            .toolbar {
                ToolbarItem(placement: .cancellationAction) {
                    Button("Done") { store.topBarSheet = nil }
                }
            }
        }
        .task { await load() }
    }

    private func list(_ sessions: [RemoteSessionSummary]) -> some View {
        List(sessions) { session in
            HStack(spacing: 10) {
                SharedToolIconView(
                    providerID: session.providerID,
                    command: session.command,
                    size: 16
                )
                VStack(alignment: .leading, spacing: 2) {
                    Text(session.title)
                        .font(.subheadline)
                        .lineLimit(1)
                    Text(Self.relativeTime(session.createdAtUnixMs))
                        .font(.caption2)
                        .foregroundStyle(.secondary)
                }
                Spacer(minLength: 8)
                if busySessionID == session.id {
                    ProgressView().controlSize(.small)
                } else {
                    Menu {
                        Button {
                            restore(session, resume: false)
                        } label: {
                            Label("Restore", systemImage: "arrow.uturn.backward")
                        }
                        if session.capabilities?.restart ?? false {
                            Button {
                                restore(session, resume: true)
                            } label: {
                                Label("Restore & Resume", systemImage: "play.circle")
                            }
                        }
                    } label: {
                        Image(systemName: "ellipsis.circle")
                            .foregroundStyle(.secondary)
                    }
                }
            }
            .contentShape(Rectangle())
        }
        .listStyle(.plain)
    }

    private var emptyState: some View {
        ContentUnavailableView(
            "No archived sessions",
            systemImage: "archivebox",
            description: Text("Sessions you stop and archive on the Mac land here.")
        )
    }

    private func errorState(_ message: String) -> some View {
        ContentUnavailableView {
            Label("Couldn't load the archive", systemImage: "exclamationmark.triangle")
        } description: {
            Text(message)
        } actions: {
            Button("Retry") {
                loadError = nil
                Task { await load() }
            }
        }
    }

    private func load() async {
        do {
            let response = try await store.client.archivedSessions(projectID: project.id)
            sessions = response.sessions
        } catch let error as RemoteMacClientError where error.statusCode == 404 {
            loadError = "This Mac's Unpeel version doesn't serve the archive yet — update the Mac app."
        } catch {
            let reason = (error as? RemoteMacClientError)?.description ?? error.localizedDescription
            loadError = reason.isEmpty ? "Could not reach your Mac." : reason
        }
    }

    private func restore(_ session: RemoteSessionSummary, resume: Bool) {
        busySessionID = session.id
        Task {
            let ok = await store.restoreArchivedSession(session, resume: resume)
            busySessionID = nil
            if ok {
                if resume {
                    // The revived session is being re-selected — take the
                    // user to it instead of leaving the sheet up.
                    store.topBarSheet = nil
                } else {
                    sessions?.removeAll { $0.id == session.id }
                }
            }
        }
    }

    private static func relativeTime(_ unixMs: Int64) -> String {
        let date = Date(timeIntervalSince1970: TimeInterval(unixMs) / 1000)
        let formatter = RelativeDateTimeFormatter()
        formatter.unitsStyle = .abbreviated
        return formatter.localizedString(for: date, relativeTo: Date())
    }
}
