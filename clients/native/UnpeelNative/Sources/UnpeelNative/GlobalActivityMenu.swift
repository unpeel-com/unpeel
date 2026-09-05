//
//  GlobalActivityMenu.swift
//  UnpeelNative
//
//  A compact, cached projection for the activity dropdown. Remote workspace
//  project trees are indexed once when their bootstrap changes; ordinary
//  SwiftUI redraws merge only the already-small active/blocked/unread rows.
//

import Combine
import Foundation
import UnpeelShared

struct WorkspaceActivityMenuSession: Equatable {
    let sessionID: String
    let title: String
    let command: String
    let projectPath: String
    let status: String
    /// Latest App alert when it is this Session's newest activity.
    let alertBody: String?
}

struct WorkspaceActivityMenuSlice: Equatable {
    var jobs: [WorkspaceActivityMenuSession] = []
    var blockers: [WorkspaceActivityMenuSession] = []
    var finished: [WorkspaceActivityMenuSession] = []

    init() {}

    /// Build once per accepted bootstrap. Project lookup/path construction is
    /// O(projects), session classification is O(sessions), and neither work is
    /// repeated by menu rendering or the spinner timer.
    init(snapshot: RemoteBootstrapSnapshot) {
        let projects = snapshot.projects.reduce(into: [String: RemoteProjectSummary]()) {
            $0[$1.id] = $1
        }
        var projectPaths: [String: String] = [:]

        func projectPath(_ projectID: String) -> String {
            if let cached = projectPaths[projectID] { return cached }
            guard projects[projectID] != nil else { return "Unknown project" }

            var names: [String] = []
            var seen = Set<String>()
            var cursor: String? = projectID
            // Malformed remote ancestry must never loop the Controller.
            while let id = cursor, let project = projects[id], seen.insert(id).inserted,
                  names.count < 32 {
                names.append(project.name)
                cursor = project.parentProjectID
            }
            let path = names.reversed().joined(separator: " › ")
            projectPaths[projectID] = path
            return path
        }

        var seen = Set<String>()
        for summary in snapshot.sessions where !summary.archived {
            guard seen.insert(summary.id).inserted else { continue }
            let item = WorkspaceActivityMenuSession(
                sessionID: summary.id,
                title: Self.title(summary.title, fallback: summary.command),
                command: summary.command,
                projectPath: projectPath(summary.projectID),
                status: Self.statusLabel(summary),
                alertBody: summary.latestAlertBody
            )
            if summary.status == .running, summary.activity == .blocked {
                blockers.append(item)
            } else if summary.status == .running,
                      summary.activity == .starting || summary.activity == .working {
                jobs.append(item)
            } else if summary.unread {
                finished.append(item)
            }
        }
    }

    init(
        local activity: ActivityMenuSessions,
        projectName: (String) -> String,
        statusLabel: (SessionEntry) -> String,
        alertBody: (SessionEntry) -> String?
    ) {
        func item(_ session: SessionEntry) -> WorkspaceActivityMenuSession {
            WorkspaceActivityMenuSession(
                sessionID: session.id,
                title: Self.title(session.label, fallback: session.presentationCommand),
                command: session.presentationCommand,
                projectPath: projectName(session.projectID),
                status: statusLabel(session),
                alertBody: alertBody(session)
            )
        }
        jobs = activity.jobs.map(item)
        blockers = activity.blockers.map(item)
        finished = activity.finished.map(item)
    }

    private static func title(_ raw: String, fallback: String) -> String {
        let title = raw.trimmingCharacters(in: .whitespacesAndNewlines)
        if !title.isEmpty { return title }
        let fallback = fallback.trimmingCharacters(in: .whitespacesAndNewlines)
        return fallback.isEmpty ? "Untitled session" : fallback
    }

    private static func statusLabel(_ session: RemoteSessionSummary) -> String {
        guard session.status == .running else { return "Exited" }
        switch session.activity {
        case .starting: return "Starting"
        case .working: return "Working"
        case .blocked: return "Blocked"
        case .done: return "Done"
        case .idle, .unknown: return "Idle"
        }
    }
}

struct GlobalActivityMenuItem: Identifiable, Equatable {
    let workspaceKey: String
    let workspaceName: String
    let workspaceTint: AppTint
    let session: WorkspaceActivityMenuSession

    var id: String { workspaceKey + "\u{1f}" + session.sessionID }
}

struct GlobalActivityMenuSessions: Equatable {
    var jobs: [GlobalActivityMenuItem] = []
    var blockers: [GlobalActivityMenuItem] = []
    var finished: [GlobalActivityMenuItem] = []

    static let empty = GlobalActivityMenuSessions()

    var sectionCount: Int {
        [blockers, jobs, finished].reduce(into: 0) { count, sessions in
            if !sessions.isEmpty { count += 1 }
        }
    }

    var rowCount: Int { jobs.count + blockers.count + finished.count }
}

/// One shared cache feeds both the titlebar and macOS menu-bar surfaces.
/// Rebuilds are coalesced to one per main-runloop turn, so a scope transition
/// that publishes several related properties still performs one merge.
@MainActor
final class GlobalActivityMenuModel: ObservableObject {
    @Published private(set) var activity = GlobalActivityMenuSessions.empty

    private weak var store: UnpeelStore?
    private let pool: WorkspacePool
    private var rows: [WorkspaceListRowModel]
    private var cancellables = Set<AnyCancellable>()
    private var rebuildScheduled = false
    private var rowsNeedRefresh = false

    init(store: UnpeelStore, pool: WorkspacePool) {
        self.store = store
        self.pool = pool
        rows = WorkspaceSwitching.orderedRows(store: store)

        store.objectWillChange
            .sink { [weak self] _ in self?.scheduleRebuild() }
            .store(in: &cancellables)
        pool.$snapshots
            .dropFirst()
            .sink { [weak self] snapshots in
                guard let self else { return }
                let known = Set(self.rows.map(\.id))
                if !Set(snapshots.keys).isSubset(of: known) {
                    self.rowsNeedRefresh = true
                }
                self.scheduleRebuild()
            }
            .store(in: &cancellables)
        store.remoteHostStore.objectWillChange
            .sink { [weak self] _ in
                self?.rowsNeedRefresh = true
                self?.scheduleRebuild()
            }
            .store(in: &cancellables)
        NotificationCenter.default.publisher(for: .unpeelWorkspaceTintChanged)
            .receive(on: DispatchQueue.main)
            .sink { [weak self] _ in
                self?.rowsNeedRefresh = true
                self?.scheduleRebuild()
            }
            .store(in: &cancellables)

        rebuild()
    }

    func refreshWorkspaceMetadata() {
        guard let store else { return }
        rows = WorkspaceSwitching.orderedRows(store: store)
        rowsNeedRefresh = false
        rebuild()
    }

    private func scheduleRebuild() {
        guard !rebuildScheduled else { return }
        rebuildScheduled = true
        DispatchQueue.main.async { [weak self] in
            guard let self else { return }
            self.rebuildScheduled = false
            self.rebuild()
        }
    }

    private func rebuild() {
        guard let store else { return }
        if rowsNeedRefresh {
            rows = WorkspaceSwitching.orderedRows(store: store)
            rowsNeedRefresh = false
        }

        let currentLocalRow = rows.first { row in
            if case let .local(_, _, isCurrentInstance, _) = row.kind {
                return isCurrentInstance
            }
            return false
        }
        let localActivity = ActivityMenuSessions(
            nodes: store.displayNodes,
            allSessions: Array(store.displaySessionsByID.values),
            jobs: store.activeJobSessions,
            finished: store.unreadJobSessions
        )
        let localSlice = WorkspaceActivityMenuSlice(
            local: localActivity,
            projectName: { store.activityProjectName($0) },
            statusLabel: { store.activityStatusLabel(for: $0) },
            alertBody: { store.latestAlertActivity(for: $0.id)?.message }
        )
        let foregroundKey = store.workspacePoolForegroundKey()

        var result = GlobalActivityMenuSessions.empty
        for row in rows {
            let slice: WorkspaceActivityMenuSlice?
            if row.id == currentLocalRow?.id {
                slice = localSlice
            } else if let cached = pool.activitySlice(forKey: row.id) {
                slice = cached
            } else if row.id == foregroundKey,
                      let snapshot = store.remoteHostRuntime.snapshot {
                // Covers the first two seconds before the deferred pool starts.
                slice = WorkspaceActivityMenuSlice(snapshot: snapshot)
            } else {
                slice = nil
            }
            guard let slice else { continue }

            func wrap(_ sessions: [WorkspaceActivityMenuSession]) -> [GlobalActivityMenuItem] {
                sessions.map {
                    GlobalActivityMenuItem(
                        workspaceKey: row.id,
                        workspaceName: row.name,
                        workspaceTint: row.tint,
                        session: $0
                    )
                }
            }
            result.blockers.append(contentsOf: wrap(slice.blockers))
            result.jobs.append(contentsOf: wrap(slice.jobs))
            result.finished.append(contentsOf: wrap(slice.finished))
        }

        if result != activity { activity = result }
    }
}
