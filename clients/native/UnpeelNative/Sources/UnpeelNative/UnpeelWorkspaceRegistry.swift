//
//  UnpeelWorkspaceRegistry.swift
//  UnpeelNative
//
//  Workspaces: multiple isolated instances of the app on one Mac. A workspace is
//  a separate running instance with its own UNPEEL_HOME (state dir, pairing
//  identity/macID, UserDefaults suite) — the same mechanism dev-blank.sh
//  uses, productized. Each workspace pairs with the phone as its own "Mac".
//
//  The released registry lives in the REAL home (~/.unpeel/profiles.json), never
//  LaunchConfig.unpeelDir: every instance, whatever its UNPEEL_HOME, must
//  see one shared registry. Workspace homes remain permanently under the legacy
//  ~/.unpeel/profiles/<slug> path — permanence matters, because provider hook
//  configs (~/.claude/settings.json, …) bake absolute script paths into
//  whichever home installed hooks last.
//

import AppKit
import Foundation

struct UnpeelWorkspaceRecord: Codable, Identifiable, Equatable {
    var id: String
    var name: String
    /// Absolute path of the workspace's UNPEEL_HOME. Minted once at create;
    /// rename never moves it (hook configs may already point into it).
    var home: String
    var createdAt: Int64
}

/// The on-disk spelling is a released compatibility contract. Keep encoding
/// the collection as `profiles` even though every source and UI surface calls
/// the product concept Workspaces now.
private struct LegacyProfilesFile: Codable {
    var version: Int
    var workspaces: [UnpeelWorkspaceRecord]

    enum CodingKeys: String, CodingKey {
        case version
        case workspaces = "profiles"
    }
}

struct UnpeelWorkspaceError: LocalizedError {
    let message: String
    init(_ message: String) { self.message = message }
    var errorDescription: String? { message }
}

enum UnpeelWorkspaceRegistry {
    /// The real ~/.unpeel — deliberately NOT LaunchConfig.unpeelDir.
    nonisolated static var realUnpeelDir: URL {
        FileManager.default.homeDirectoryForCurrentUser
            .appendingPathComponent(".unpeel")
    }

    nonisolated static var registryURL: URL {
        // Legacy filename retained so released native builds and newly named
        // clients continue to share one registry.
        realUnpeelDir.appendingPathComponent("profiles.json")
    }

    nonisolated static var workspaceHomesRoot: URL {
        // Never move existing homes: hook configs contain absolute paths and
        // AppDefaults derives each workspace's defaults suite from this path.
        realUnpeelDir.appendingPathComponent("profiles")
    }

    /// Pure codec entry points keep the released JSON shape directly testable.
    nonisolated static func decodeRegistry(_ data: Data) throws -> [UnpeelWorkspaceRecord] {
        try JSONDecoder().decode(LegacyProfilesFile.self, from: data).workspaces
    }

    nonisolated static func encodeRegistry(_ workspaces: [UnpeelWorkspaceRecord]) throws -> Data {
        try JSONEncoder().encode(LegacyProfilesFile(version: 1, workspaces: workspaces))
    }

    nonisolated static func load() -> [UnpeelWorkspaceRecord] {
        guard let data = try? Data(contentsOf: registryURL),
              let workspaces = try? decodeRegistry(data)
        else { return [] }
        return workspaces
    }

    nonisolated static func save(_ workspaces: [UnpeelWorkspaceRecord]) {
        do {
            try FileManager.default.createDirectory(
                at: registryURL.deletingLastPathComponent(),
                withIntermediateDirectories: true
            )
            let data = try encodeRegistry(workspaces)
            try data.write(to: registryURL, options: .atomic)
            try FileManager.default.setAttributes(
                [.posixPermissions: 0o600],
                ofItemAtPath: registryURL.path
            )
        } catch {
            NSLog("[UnpeelNative] failed to persist workspace registry: \(error)")
        }
    }

    @discardableResult
    nonisolated static func create(name: String) throws -> UnpeelWorkspaceRecord {
        let trimmed = name.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !trimmed.isEmpty else { throw UnpeelWorkspaceError("Give the workspace a name.") }
        // Re-read right before mutating: another instance may have edited the
        // registry (atomic last-writer-wins is the concurrency model).
        var workspaces = load()
        let slug = uniqueSlug(for: trimmed, existing: workspaces)
        let home = workspaceHomesRoot.appendingPathComponent(slug)
        try FileManager.default.createDirectory(at: home, withIntermediateDirectories: true)
        let record = UnpeelWorkspaceRecord(
            id: UUID().uuidString.lowercased(),
            name: trimmed,
            home: home.path,
            createdAt: Int64(Date().timeIntervalSince1970 * 1000)
        )
        workspaces.append(record)
        save(workspaces)
        return record
    }

    nonisolated static func rename(id: String, to name: String) {
        let trimmed = name.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !trimmed.isEmpty else { return }
        var workspaces = load()
        guard let index = workspaces.firstIndex(where: { $0.id == id }) else { return }
        workspaces[index].name = trimmed
        save(workspaces)
    }

    /// Forget a workspace. `deleteData` also removes its home dir — refuse
    /// while it is running (callers check `UnpeelWorkspaceLauncher.runningPid`).
    nonisolated static func remove(id: String, deleteData: Bool) {
        var workspaces = load()
        guard let index = workspaces.firstIndex(where: { $0.id == id }) else { return }
        let record = workspaces.remove(at: index)
        save(workspaces)
        if deleteData {
            // Only ever delete homes we minted under the managed root — a
            // hand-registered entry pointing elsewhere is not ours to rm -rf.
            let normalized = normalizePath(record.home)
            if normalized.hasPrefix(normalizePath(workspaceHomesRoot.path) + "/") {
                try? FileManager.default.removeItem(atPath: record.home)
            }
        }
    }

    nonisolated static func slugify(_ name: String) -> String {
        var slug = ""
        var lastWasDash = true // suppress leading dashes
        for scalar in name.lowercased().unicodeScalars {
            if CharacterSet.alphanumerics.contains(scalar), scalar.isASCII {
                slug.unicodeScalars.append(scalar)
                lastWasDash = false
            } else if !lastWasDash {
                slug.append("-")
                lastWasDash = true
            }
        }
        while slug.hasSuffix("-") { slug.removeLast() }
        return slug.isEmpty ? "workspace" : slug
    }

    nonisolated private static func uniqueSlug(
        for name: String,
        existing: [UnpeelWorkspaceRecord]
    ) -> String {
        let base = slugify(name)
        let taken = Set(existing.map { normalizePath($0.home) })
        var candidate = base
        var counter = 2
        while taken.contains(normalizePath(workspaceHomesRoot.appendingPathComponent(candidate).path))
            || FileManager.default.fileExists(
                atPath: workspaceHomesRoot.appendingPathComponent(candidate).path
            )
        {
            candidate = "\(base)-\(counter)"
            counter += 1
        }
        return candidate
    }

    nonisolated static func normalizePath(_ path: String) -> String {
        URL(fileURLWithPath: (path as NSString).expandingTildeInPath)
            .standardizedFileURL
            .resolvingSymlinksInPath()
            .path
    }
}

/// Which workspace THIS process is. Resolved from UNPEEL_HOME against the
/// registry; the default instance (no UNPEEL_HOME) is the implicit workspace.
enum UnpeelWorkspaceContext {
    private nonisolated static let defaultWorkspaceNameKey =
        "unpeel.native.defaultWorkspaceName"

    nonisolated static var isDefaultInstance: Bool {
        guard let home = ProcessInfo.processInfo.environment["UNPEEL_HOME"] else { return true }
        return home.trimmingCharacters(in: .whitespaces).isEmpty
    }

    /// Registry entry for this instance's UNPEEL_HOME; nil for the default
    /// instance and for unregistered homes (dev-blank runs). Re-reads the
    /// registry per call so renames from another instance apply live.
    nonisolated static func currentWorkspace() -> UnpeelWorkspaceRecord? {
        guard !isDefaultInstance,
              let env = ProcessInfo.processInfo.environment["UNPEEL_HOME"]
        else { return nil }
        let home = UnpeelWorkspaceRegistry.normalizePath(env)
        return UnpeelWorkspaceRegistry.load().first {
            UnpeelWorkspaceRegistry.normalizePath($0.home) == home
        }
    }

    /// nil for the default instance. Unregistered UNPEEL_HOMEs (dev-blank)
    /// fall back to the dir name so even those are tellable apart.
    nonisolated static var displayName: String? {
        guard !isDefaultInstance else { return nil }
        if let workspace = currentWorkspace() { return workspace.name }
        guard let env = ProcessInfo.processInfo.environment["UNPEEL_HOME"] else { return nil }
        return URL(fileURLWithPath: (env as NSString).expandingTildeInPath).lastPathComponent
    }

    /// Optional user alias for the implicit/default workspace. Keeping this
    /// separate from `displayName` preserves nil as the default-instance
    /// sentinel used by older callers. Other workspace instances read the
    /// standard suite so the sidebar presents one shared name.
    nonisolated static var defaultWorkspaceName: String? {
        UserDefaults.standard.string(forKey: defaultWorkspaceNameKey)?
            .trimmingCharacters(in: .whitespacesAndNewlines)
            .nilIfEmpty
    }

    @discardableResult
    nonisolated static func renameDefaultWorkspace(to rawName: String) -> Bool {
        let name = rawName.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !name.isEmpty else { return false }
        UserDefaults.standard.set(name, forKey: defaultWorkspaceNameKey)
        return true
    }

    /// The single choke point for the name this instance advertises to
    /// phones (pairing, bootstrap, Bonjour): workspace name for an isolated
    /// instance, otherwise the Mac's host name.
    nonisolated static var advertisedHostName: String {
        displayName
            ?? defaultWorkspaceName
            ?? Host.current().localizedName
            ?? Host.current().name
            ?? "Mac"
    }
}

private extension String {
    var nilIfEmpty: String? { isEmpty ? nil : self }
}

/// Controller-local, user-chosen display order for the unified workspace
/// list (Settings ▸ Workspaces and the sidebar picker render the SAME
/// order). Keys are kind-prefixed so local homes and remote host ids can
/// never collide: `local:<normalized home>`, `host:<hostID>`, `ssh:<id>`.
/// Unknown keys keep their natural build order after the saved ones; saving
/// rewrites the full list, so removed workspaces age out on their own.
enum WorkspaceListOrder {
    nonisolated static let defaultsKey = "unpeel.native.workspaceOrder"

    nonisolated static func localKey(home: String) -> String {
        "local:\(UnpeelWorkspaceRegistry.normalizePath(home))"
    }

    nonisolated static func pairedKey(hostID: String) -> String {
        "host:\(hostID)"
    }

    nonisolated static func sshKey(id: String) -> String {
        "ssh:\(id)"
    }

    nonisolated static func load() -> [String] {
        AppDefaults.shared.stringArray(forKey: defaultsKey) ?? []
    }

    nonisolated static func save(_ keys: [String]) {
        AppDefaults.shared.set(keys, forKey: defaultsKey)
    }

    /// Stable sort by saved position; unsaved keys follow in build order.
    nonisolated static func apply<Row>(
        to rows: [Row],
        key: (Row) -> String
    ) -> [Row] {
        let saved = load()
        let position = Dictionary(
            uniqueKeysWithValues: saved.enumerated().map { ($1, $0) }
        )
        return rows.enumerated().sorted { left, right in
            switch (position[key(left.element)], position[key(right.element)]) {
            case let (.some(a), .some(b)): a < b
            case (.some, .none): true
            case (.none, .some): false
            case (.none, .none): left.offset < right.offset
            }
        }.map(\.element)
    }
}

/// Launching and liveness of workspace instances. A per-home `app.pid`
/// (written at startup) is the running marker; identity is verified against
/// the kernel-reported process start time before trusting it — same pid-reuse
/// discipline as the hosted-session manifests.
enum UnpeelWorkspaceLauncher {
    struct AppPidFile: Codable {
        var pid: Int32
        var pidStartedAt: UInt64?
    }

    nonisolated private static let pidStartToleranceMs: UInt64 = 10_000

    nonisolated static func pidFileURL(home: URL) -> URL {
        home.appendingPathComponent("app.pid")
    }

    /// Pid of the live instance owning `home`, or nil (missing/stale/
    /// unverifiable pidfile).
    nonisolated static func runningPid(home: URL) -> Int32? {
        guard let data = try? Data(contentsOf: pidFileURL(home: home)),
              let file = try? JSONDecoder().decode(AppPidFile.self, from: data),
              file.pid > 1,
              let actual = UnpeelStore.processStartTimeMs(file.pid)
        else { return nil }
        guard let recorded = file.pidStartedAt else { return nil }
        let drift = actual > recorded ? actual - recorded : recorded - actual
        return drift <= pidStartToleranceMs ? file.pid : nil
    }

    /// Written by every instance for its own home at startup.
    nonisolated static func writeOwnPidFile() {
        let home = LaunchConfig.unpeelDir
        let pid = ProcessInfo.processInfo.processIdentifier
        let file = AppPidFile(pid: pid, pidStartedAt: UnpeelStore.processStartTimeMs(pid))
        do {
            try FileManager.default.createDirectory(at: home, withIntermediateDirectories: true)
            let data = try JSONEncoder().encode(file)
            try data.write(to: pidFileURL(home: home), options: .atomic)
        } catch {
            NSLog("[UnpeelNative] failed to write app.pid: \(error)")
        }
    }

    nonisolated static func removeOwnPidFile() {
        try? FileManager.default.removeItem(at: pidFileURL(home: LaunchConfig.unpeelDir))
    }

    /// True when another live process already owns this instance's home.
    nonisolated static func otherInstanceOwnsCurrentHome() -> Bool {
        guard let pid = runningPid(home: LaunchConfig.unpeelDir) else { return false }
        return pid != ProcessInfo.processInfo.processIdentifier
    }

    /// Ask a running instance owning `home` to show its window: POST
    /// /show-window to every port in that home's `app-ports` (that home's own
    /// hook-server registry — one line per instance). A running instance can
    /// be windowless, so activation alone won't reopen one. Fire-and-forget:
    /// a dead port costs nothing and stale entries are pruned by the
    /// registry's owners.
    nonisolated static func showWindow(home: URL) {
        let portsFile = home.appendingPathComponent("app-ports")
        guard let contents = try? String(contentsOf: portsFile, encoding: .utf8)
        else { return }
        for line in contents.split(whereSeparator: \.isNewline) {
            guard let port = Int(line.trimmingCharacters(in: .whitespaces)),
                  (1...65535).contains(port),
                  let url = URL(string: "http://127.0.0.1:\(port)/show-window")
            else { continue }
            var request = URLRequest(url: url, timeoutInterval: 2)
            request.httpMethod = "POST"
            URLSession.shared.dataTask(with: request).resume()
        }
    }

    /// Launch a workspace as a second instance of this same app binary.
    /// Direct-exec on purpose: `open`/NSWorkspace neither forwards env nor
    /// starts a second instance of an already-running bundle id.
    /// `hidden` starts the instance WINDOWLESS (menu-bar agent state) — used
    /// when the launch only exists to serve the workspace (pairing) and a
    /// window would steal the user's focus; /show-window surfaces it later.
    @MainActor
    static func launch(_ workspace: UnpeelWorkspaceRecord, hidden: Bool = false) throws {
        let home = URL(fileURLWithPath: workspace.home, isDirectory: true)
        if runningPid(home: home) != nil {
            throw UnpeelWorkspaceError("\(workspace.name) is already running.")
        }
        guard let executable = Bundle.main.executableURL else {
            throw UnpeelWorkspaceError("Could not locate the app executable.")
        }
        try FileManager.default.createDirectory(at: home, withIntermediateDirectories: true)
        var environment = ProcessInfo.processInfo.environment
        environment["UNPEEL_HOME"] = workspace.home
        // Test/snapshot vars arm capture harnesses — never let them leak
        // into a user-facing instance.
        for key in environment.keys where key.hasPrefix("UNPEEL_TEST_") || key.hasPrefix("UNPEEL_SNAPSHOT") {
            environment.removeValue(forKey: key)
        }
        if hidden {
            environment["UNPEEL_LAUNCH_HIDDEN"] = "1"
        } else {
            // Never inherit a hidden marker from a hidden-launched parent.
            environment.removeValue(forKey: "UNPEEL_LAUNCH_HIDDEN")
        }
        let process = Process()
        process.executableURL = executable
        process.environment = environment
        process.standardOutput = FileHandle.nullDevice
        process.standardError = FileHandle.nullDevice
        try process.run()
        // No waitUntilExit: the child is a full GUI app that outlives us.
    }
}
