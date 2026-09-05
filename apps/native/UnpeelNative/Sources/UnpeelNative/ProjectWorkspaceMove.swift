//
//  ProjectWorkspaceMove.swift
//  UnpeelNative
//
//  Move a top-level project — and the sessions filed under it — from one
//  local workspace home to another. Workspaces are isolated UNPEEL_HOME
//  trees (docs/agents/workspaces.md); this is a same-Mac file transfer of
//  the shared on-disk contract, not a protocol verb.
//
//  Sessions stay the same ids. Live hosts keep running: a same-volume
//  rename of app-sessions/<id> leaves the PTY, output.bin fd, and
//  session.sock inode intact, which is the same survival model as an
//  app restart. Hook broadcasts stay on the source home's ports until
//  that Host is later replaced.
//

import Foundation

/// A local workspace the project context menu can file into.
struct WorkspaceMoveTarget: Identifiable, Equatable, Hashable {
    /// Normalized home path — stable identity across instances.
    let id: String
    let name: String
    /// Home path as the destination instance receives it (suite hashing
    /// and `selectLocalWorkspace` both want this spelling).
    let home: String
}

enum ProjectWorkspaceMove {
    struct Outcome: Equatable {
        let rootProjectID: String
        let rootProjectName: String
        let projectIDs: Set<String>
        let sessionIDs: [String]
    }

    enum MoveError: Error, Equatable, LocalizedError {
        case sameHome
        case projectNotFound
        case destAlreadyHasProject
        case destAlreadyHasPath
        case destAlreadyHasSession(String)
        case destStateWriteFailed
        case sourceStateWriteFailed

        var errorDescription: String? {
            switch self {
            case .sameHome:
                return "That project is already in this workspace."
            case .projectNotFound:
                return "Couldn't find that project."
            case .destAlreadyHasProject:
                return "The other workspace already has this project."
            case .destAlreadyHasPath:
                return "The other workspace already has a project at that folder."
            case .destAlreadyHasSession:
                return "The other workspace already has one of these sessions."
            case .destStateWriteFailed:
                return "Couldn't update the destination workspace."
            case .sourceStateWriteFailed:
                return "The project landed in the other workspace, but this workspace still lists it."
            }
        }
    }

    /// Disk-only transfer between two UNPEEL_HOME trees. Does not touch
    /// UserDefaults overlays (the store copies those after a successful
    /// move). Live session dirs are renamed in place — do not stop hosts.
    @discardableResult
    static func move(
        projectID: String,
        from sourceHome: URL,
        to destHome: URL
    ) throws -> Outcome {
        let source = sourceHome.standardizedFileURL
        let dest = destHome.standardizedFileURL
        guard source.path != dest.path else { throw MoveError.sameHome }

        let sourceStateURL = source.appendingPathComponent("app-state.json")
        let destStateURL = dest.appendingPathComponent("app-state.json")
        let sourceProjects = loadProjects(at: sourceStateURL)
        guard let root = sourceProjects.first(where: { $0.id == projectID }) else {
            throw MoveError.projectNotFound
        }
        let subtreeIDs = descendantProjectIDs(rootID: projectID, in: sourceProjects)
        let movingProjects = sourceProjects.filter { subtreeIDs.contains($0.id) }
        let destProjects = loadProjects(at: destStateURL)
        let destIDs = Set(destProjects.map(\.id))
        if destIDs.contains(projectID) {
            throw MoveError.destAlreadyHasProject
        }
        let destPaths = Set(
            destProjects
                .filter { $0.parentProjectID == nil }
                .map { normalizedPath($0.path) }
        )
        if destPaths.contains(normalizedPath(root.path)) {
            throw MoveError.destAlreadyHasPath
        }
        if movingProjects.contains(where: { destIDs.contains($0.id) }) {
            throw MoveError.destAlreadyHasProject
        }

        let knownSourceIDs = Set(sourceProjects.map(\.id))
        let sessionIDs = try collectSessionIDs(
            home: source,
            subtreeIDs: subtreeIDs,
            knownProjectIDs: knownSourceIDs
        )
        let destSessionsDir = dest.appendingPathComponent("app-sessions", isDirectory: true)
        for sessionID in sessionIDs {
            let destDir = destSessionsDir.appendingPathComponent(sessionID, isDirectory: true)
            if FileManager.default.fileExists(atPath: destDir.path) {
                throw MoveError.destAlreadyHasSession(sessionID)
            }
        }

        try FileManager.default.createDirectory(
            at: destSessionsDir,
            withIntermediateDirectories: true
        )
        var moved: [String] = []
        do {
            for sessionID in sessionIDs {
                let srcDir = source
                    .appendingPathComponent("app-sessions", isDirectory: true)
                    .appendingPathComponent(sessionID, isDirectory: true)
                let destDir = destSessionsDir.appendingPathComponent(
                    sessionID,
                    isDirectory: true
                )
                guard FileManager.default.fileExists(atPath: srcDir.path) else { continue }
                try FileManager.default.moveItem(at: srcDir, to: destDir)
                moved.append(sessionID)
            }
        } catch {
            rollbackMovedSessions(moved, from: destSessionsDir, to: source)
            throw error
        }

        let sourceOrders = loadSessionOrders(home: source)
        let sourceProjectOrder = loadProjectOrder(home: source)
        let sourceState = loadJSONObject(at: sourceStateURL) ?? [:]

        let destWrote = PresetStateFile.edit(at: destStateURL) { object in
            mergeProjects(movingProjects, into: &object)
            mergePinnedSessions(
                from: sourceState,
                projectIDs: subtreeIDs,
                into: &object
            )
            mergeStringMap(
                key: "session_sort_modes",
                from: sourceState,
                keys: subtreeIDs,
                into: &object
            )
            mergeMcpOrchestrators(
                from: sourceState,
                sessionIDs: Set(sessionIDs),
                into: &object
            )
            mergeBlockedProjects(
                from: sourceState,
                projectIDs: subtreeIDs,
                into: &object
            )
            if object["active_project_id"] == nil
                || object["active_project_id"] is NSNull
            {
                object["active_project_id"] = projectID
            }
        }
        if !destWrote {
            rollbackMovedSessions(moved, from: destSessionsDir, to: source)
            throw MoveError.destStateWriteFailed
        }
        mergeSessionOrders(
            home: dest,
            adding: Dictionary(
                uniqueKeysWithValues: subtreeIDs.compactMap { id in
                    sourceOrders[id].map { (id, $0) }
                }
            )
        )
        appendProjectOrder(home: dest, ids: orderedProjectIDs(
            subtreeIDs: subtreeIDs,
            sourceOrder: sourceProjectOrder,
            rootID: projectID
        ))

        let sourceWrote = PresetStateFile.edit(at: sourceStateURL) { object in
            stripProjects(subtreeIDs, from: &object)
            stripPinnedSessions(projectIDs: subtreeIDs, from: &object)
            stripStringMap(key: "session_sort_modes", keys: subtreeIDs, from: &object)
            stripMcpOrchestrators(sessionIDs: Set(sessionIDs), from: &object)
            stripBlockedProjects(projectIDs: subtreeIDs, from: &object)
            if (object["active_project_id"] as? String).map(subtreeIDs.contains) == true {
                object["active_project_id"] = NSNull()
            }
        }
        stripSessionOrders(home: source, projectIDs: subtreeIDs)
        stripProjectOrder(home: source, ids: subtreeIDs)
        if !sourceWrote {
            throw MoveError.sourceStateWriteFailed
        }

        return Outcome(
            rootProjectID: projectID,
            rootProjectName: root.name,
            projectIDs: subtreeIDs,
            sessionIDs: sessionIDs
        )
    }

    /// Session ids that would move with `projectID` — used to stop hosts
    /// before the transfer.
    static func sessionIDs(
        projectID: String,
        in home: URL
    ) -> [String] {
        let projects = loadProjects(at: home.appendingPathComponent("app-state.json"))
        guard projects.contains(where: { $0.id == projectID }) else { return [] }
        let subtree = descendantProjectIDs(rootID: projectID, in: projects)
        return (try? collectSessionIDs(
            home: home,
            subtreeIDs: subtree,
            knownProjectIDs: Set(projects.map(\.id))
        )) ?? []
    }

    static func liveSessionIDs(projectID: String, in home: URL) -> [String] {
        sessionIDs(projectID: projectID, in: home).filter {
            sessionIsLive(home: home, sessionID: $0)
        }
    }

    // MARK: - Discovery

    private struct ProjectRow {
        let id: String
        let name: String
        let path: String
        let parentProjectID: String?
        let raw: [String: Any]
    }

    private static func loadProjects(at url: URL) -> [ProjectRow] {
        guard let object = loadJSONObject(at: url),
              let rows = object["projects"] as? [[String: Any]]
        else { return [] }
        return rows.compactMap { raw in
            guard let id = raw["id"] as? String,
                  let name = raw["name"] as? String,
                  let path = raw["path"] as? String
            else { return nil }
            return ProjectRow(
                id: id,
                name: name,
                path: path,
                parentProjectID: raw["parent_project_id"] as? String,
                raw: raw
            )
        }
    }

    static func descendantProjectIDs(rootID: String, in projects: [Project]) -> Set<String> {
        descendantProjectIDs(
            rootID: rootID,
            parentOf: Dictionary(
                uniqueKeysWithValues: projects.map { ($0.id, $0.parentProjectID) }
            )
        )
    }

    private static func descendantProjectIDs(
        rootID: String,
        in rows: [ProjectRow]
    ) -> Set<String> {
        descendantProjectIDs(
            rootID: rootID,
            parentOf: Dictionary(uniqueKeysWithValues: rows.map { ($0.id, $0.parentProjectID) })
        )
    }

    private static func descendantProjectIDs(
        rootID: String,
        parentOf: [String: String?]
    ) -> Set<String> {
        var result: Set<String> = [rootID]
        var stack = [rootID]
        while let current = stack.popLast() {
            for (id, parent) in parentOf where parent == current && !result.contains(id) {
                result.insert(id)
                stack.append(id)
            }
        }
        return result
    }

    private static func collectSessionIDs(
        home: URL,
        subtreeIDs: Set<String>,
        knownProjectIDs: Set<String>
    ) throws -> [String] {
        let sessionsDir = home.appendingPathComponent("app-sessions", isDirectory: true)
        let contents = (try? FileManager.default.contentsOfDirectory(
            at: sessionsDir,
            includingPropertiesForKeys: [.isDirectoryKey],
            options: [.skipsHiddenFiles]
        )) ?? []
        var ids: [String] = []
        for dir in contents {
            let isDir = (try? dir.resourceValues(forKeys: [.isDirectoryKey]).isDirectory) ?? false
            guard isDir else { continue }
            let sessionID = dir.lastPathComponent
            guard let bucket = sessionBucket(
                dir: dir,
                knownProjectIDs: knownProjectIDs
            ), subtreeIDs.contains(bucket)
            else { continue }
            ids.append(sessionID)
        }
        return ids.sorted()
    }

    /// Same bucket keying as UnpeelStore.rebuildTree / removeProject: a
    /// valid override target wins over the manifest project.
    private static func sessionBucket(
        dir: URL,
        knownProjectIDs: Set<String>
    ) -> String? {
        let overrideURL = dir.appendingPathComponent("project-override.json")
        if let data = try? Data(contentsOf: overrideURL),
           let object = (try? JSONSerialization.jsonObject(with: data)) as? [String: Any],
           let override = object["project_id"] as? String,
           knownProjectIDs.contains(override)
        {
            return override
        }
        guard let data = try? Data(contentsOf: dir.appendingPathComponent("manifest.json")),
              let manifest = try? JSONDecoder().decode(HostedSessionManifest.self, from: data)
        else { return nil }
        return manifest.session.projectID
    }

    static func sessionIsLive(home: URL, sessionID: String) -> Bool {
        let dir = home
            .appendingPathComponent("app-sessions", isDirectory: true)
            .appendingPathComponent(sessionID, isDirectory: true)
        guard let data = try? Data(contentsOf: dir.appendingPathComponent("manifest.json")),
              let manifest = try? JSONDecoder().decode(HostedSessionManifest.self, from: data),
              manifest.state == "running"
        else { return false }
        let exists = UnpeelStore.hostedChildProcessExists(manifest.pid)
        if exists == false { return false }
        return UnpeelStore.manifestPidIdentity(manifest) != .notOurs
    }

    // MARK: - JSON helpers

    private static func loadJSONObject(at url: URL) -> [String: Any]? {
        guard let data = try? Data(contentsOf: url),
              let object = try? JSONSerialization.jsonObject(with: data)
        else { return nil }
        return object as? [String: Any]
    }

    private static func mergeProjects(_ rows: [ProjectRow], into object: inout [String: Any]) {
        var projects = (object["projects"] as? [[String: Any]]) ?? []
        let existing = Set(projects.compactMap { $0["id"] as? String })
        for row in rows where !existing.contains(row.id) {
            projects.append(row.raw)
        }
        object["projects"] = projects
    }

    private static func stripProjects(_ ids: Set<String>, from object: inout [String: Any]) {
        var projects = (object["projects"] as? [[String: Any]]) ?? []
        projects.removeAll { ($0["id"] as? String).map(ids.contains) == true }
        object["projects"] = projects
    }

    private static func mergePinnedSessions(
        from source: [String: Any],
        projectIDs: Set<String>,
        into dest: inout [String: Any]
    ) {
        var destPins = groupedPins(dest["pinned_sessions"])
        let sourcePins = groupedPins(source["pinned_sessions"])
        for id in projectIDs {
            if let rows = sourcePins[id], !rows.isEmpty {
                destPins[id] = rows
            }
        }
        dest["pinned_sessions"] = destPins
    }

    private static func stripPinnedSessions(
        projectIDs: Set<String>,
        from object: inout [String: Any]
    ) {
        var pins = groupedPins(object["pinned_sessions"])
        for id in projectIDs { pins.removeValue(forKey: id) }
        object["pinned_sessions"] = pins
    }

    private static func groupedPins(_ raw: Any?) -> [String: [[String: Any]]] {
        if let groups = raw as? [String: Any] {
            return groups.reduce(into: [:]) { result, item in
                result[item.key] = (item.value as? [Any])?.compactMap { $0 as? [String: Any] }
            }
        }
        if let rows = raw as? [[String: Any]] {
            return Dictionary(grouping: rows, by: { $0["project_id"] as? String ?? "" })
                .filter { !$0.key.isEmpty }
        }
        return [:]
    }

    private static func mergeStringMap(
        key: String,
        from source: [String: Any],
        keys: Set<String>,
        into dest: inout [String: Any]
    ) {
        var map = (dest[key] as? [String: Any]) ?? [:]
        let sourceMap = (source[key] as? [String: Any]) ?? [:]
        for id in keys {
            if let value = sourceMap[id] { map[id] = value }
        }
        dest[key] = map
    }

    private static func stripStringMap(
        key: String,
        keys: Set<String>,
        from object: inout [String: Any]
    ) {
        var map = (object[key] as? [String: Any]) ?? [:]
        for id in keys { map.removeValue(forKey: id) }
        object[key] = map
    }

    private static func mergeMcpOrchestrators(
        from source: [String: Any],
        sessionIDs: Set<String>,
        into dest: inout [String: Any]
    ) {
        var map = (dest["mcp_orchestrators"] as? [String: Any]) ?? [:]
        let sourceMap = (source["mcp_orchestrators"] as? [String: Any]) ?? [:]
        for id in sessionIDs {
            if let value = sourceMap[id] { map[id] = value }
        }
        dest["mcp_orchestrators"] = map
    }

    private static func stripMcpOrchestrators(
        sessionIDs: Set<String>,
        from object: inout [String: Any]
    ) {
        var map = (object["mcp_orchestrators"] as? [String: Any]) ?? [:]
        for id in sessionIDs { map.removeValue(forKey: id) }
        object["mcp_orchestrators"] = map
    }

    private static func mergeBlockedProjects(
        from source: [String: Any],
        projectIDs: Set<String>,
        into dest: inout [String: Any]
    ) {
        var ids = (dest["mcp_blocked_projects"] as? [String]) ?? []
        let sourceIDs = Set((source["mcp_blocked_projects"] as? [String]) ?? [])
        for id in projectIDs where sourceIDs.contains(id) && !ids.contains(id) {
            ids.append(id)
        }
        dest["mcp_blocked_projects"] = ids
    }

    private static func stripBlockedProjects(
        projectIDs: Set<String>,
        from object: inout [String: Any]
    ) {
        var ids = (object["mcp_blocked_projects"] as? [String]) ?? []
        ids.removeAll(where: projectIDs.contains)
        object["mcp_blocked_projects"] = ids
    }

    // MARK: - Order files

    private static func sessionOrderURL(home: URL) -> URL {
        home.appendingPathComponent("session-order.json")
    }

    private static func projectOrderURL(home: URL) -> URL {
        home.appendingPathComponent("project-order.json")
    }

    private static func loadSessionOrders(home: URL) -> [String: [String]] {
        let url = sessionOrderURL(home: home)
        guard let data = try? Data(contentsOf: url),
              let root = (try? JSONSerialization.jsonObject(with: data)) as? [String: Any]
        else { return [:] }
        return root.reduce(into: [:]) { result, item in
            if let ids = item.value as? [String] { result[item.key] = ids }
        }
    }

    private static func loadProjectOrder(home: URL) -> [String] {
        let url = projectOrderURL(home: home)
        guard let data = try? Data(contentsOf: url),
              let ids = (try? JSONSerialization.jsonObject(with: data)) as? [String]
        else { return [] }
        return ids
    }

    private static func mergeSessionOrders(home: URL, adding: [String: [String]]) {
        guard !adding.isEmpty else { return }
        let url = sessionOrderURL(home: home)
        _ = PresetStateFile.withExclusiveLock(on: url) {
            var root = loadSessionOrders(home: home)
            for (projectID, ids) in adding where !ids.isEmpty {
                root[projectID] = ids
            }
            writeJSON(root, to: url)
        }
    }

    private static func stripSessionOrders(home: URL, projectIDs: Set<String>) {
        let url = sessionOrderURL(home: home)
        _ = PresetStateFile.withExclusiveLock(on: url) {
            var root = loadSessionOrders(home: home)
            for id in projectIDs { root.removeValue(forKey: id) }
            writeJSON(root, to: url)
        }
    }

    private static func appendProjectOrder(home: URL, ids: [String]) {
        guard !ids.isEmpty else { return }
        let url = projectOrderURL(home: home)
        _ = PresetStateFile.withExclusiveLock(on: url) {
            var order = loadProjectOrder(home: home)
            for id in ids where !order.contains(id) {
                order.append(id)
            }
            writeJSONArray(order, to: url)
        }
    }

    private static func stripProjectOrder(home: URL, ids: Set<String>) {
        let url = projectOrderURL(home: home)
        _ = PresetStateFile.withExclusiveLock(on: url) {
            let order = loadProjectOrder(home: home).filter { !ids.contains($0) }
            writeJSONArray(order, to: url)
        }
    }

    private static func orderedProjectIDs(
        subtreeIDs: Set<String>,
        sourceOrder: [String],
        rootID: String
    ) -> [String] {
        var seen = Set<String>()
        var ids: [String] = []
        for id in sourceOrder where subtreeIDs.contains(id) && seen.insert(id).inserted {
            ids.append(id)
        }
        if seen.insert(rootID).inserted {
            ids.insert(rootID, at: 0)
        }
        for id in subtreeIDs.sorted() where seen.insert(id).inserted {
            ids.append(id)
        }
        return ids
    }

    private static func writeJSON(_ object: [String: [String]], to url: URL) {
        guard JSONSerialization.isValidJSONObject(object),
              let data = try? JSONSerialization.data(withJSONObject: object)
        else { return }
        try? FileManager.default.createDirectory(
            at: url.deletingLastPathComponent(),
            withIntermediateDirectories: true
        )
        try? data.write(to: url, options: .atomic)
    }

    private static func writeJSONArray(_ ids: [String], to url: URL) {
        guard let data = try? JSONSerialization.data(withJSONObject: ids) else { return }
        try? FileManager.default.createDirectory(
            at: url.deletingLastPathComponent(),
            withIntermediateDirectories: true
        )
        try? data.write(to: url, options: .atomic)
    }

    private static func rollbackMovedSessions(
        _ sessionIDs: [String],
        from destSessionsDir: URL,
        to sourceHome: URL
    ) {
        let sourceSessions = sourceHome.appendingPathComponent(
            "app-sessions",
            isDirectory: true
        )
        try? FileManager.default.createDirectory(
            at: sourceSessions,
            withIntermediateDirectories: true
        )
        for sessionID in sessionIDs {
            let destDir = destSessionsDir.appendingPathComponent(sessionID, isDirectory: true)
            let srcDir = sourceSessions.appendingPathComponent(sessionID, isDirectory: true)
            guard FileManager.default.fileExists(atPath: destDir.path),
                  !FileManager.default.fileExists(atPath: srcDir.path)
            else { continue }
            try? FileManager.default.moveItem(at: destDir, to: srcDir)
        }
    }

    private static func normalizedPath(_ path: String) -> String {
        URL(fileURLWithPath: path)
            .standardizedFileURL
            .resolvingSymlinksInPath()
            .path
    }
}
