//
//  ComputerEngineManager.swift
//  UnpeelNative
//
//  Owns the computer-use engine daemon: `cua-driver serve --embedded --socket
//  <home>/computer/daemon.sock`, spawned as a DIRECT child of the app. Being
//  the app's child is the whole point (cua-driver's embedding contract,
//  Skills/cua-driver/EMBEDDING.md): macOS TCC attributes Accessibility and
//  Screen Recording to the responsible process at the top of the launch
//  chain, so the daemon inherits Unpeel.app's grants — one grant, one
//  Settings entry, no second prompt. Never launch it via `open`/NSWorkspace
//  (LaunchServices breaks responsibility inheritance).
//
//  The unified MCP server (computer_mcp.rs) talks to this daemon with
//  one-shot `cua-driver call … --socket` invocations; it never spawns a
//  daemon itself — a daemon spawned from a session host that outlived an app
//  restart would have a murky responsibility chain, and the docs are explicit
//  that the GUI app must be the spawner.
//
//  Policy: run exactly while the Computer Use experimental flag is on AND
//  computer access (app-state.json `computer_default_access`) is not Off.
//  The daemon runs in `unrestricted` mode via the documented two-env
//  contract — Unpeel owns the user-facing approval flow (Off/Ask/Allow gate
//  + per-session Ask alert), so the driver's own runtime approvals would be
//  a second, headless prompt layer with no one to answer it.
//
//  TCC answers are cached per process, so when a grant changes (the user
//  flips something in System Settings) the daemon must be restarted to see
//  it — `sync()` fingerprints the grants and restarts on change.
//

import AppKit

@MainActor
final class ComputerEngineManager {
    static let shared = ComputerEngineManager()

    private var process: Process?
    private var stopping = false
    private var rapidFailures = 0
    private var gaveUp = false
    private var lastLaunchAt: Date = .distantPast
    private var pendingRestart: DispatchWorkItem?
    private var activationObserver: NSObjectProtocol?
    private var defaultsObserver: NSObjectProtocol?
    /// (accessibility, screenRecording) at the moment the daemon spawned;
    /// a change means the daemon's cached TCC answers are stale.
    private var grantsAtLaunch: (ax: Bool, sr: Bool)?

    private let restartDelay: TimeInterval = 2
    private let maxRapidFailures = 5
    private let rapidExitWindow: TimeInterval = 10

    static var socketPath: String {
        LaunchConfig.unpeelDir
            .appendingPathComponent("computer")
            .appendingPathComponent("daemon.sock").path
    }

    private static var stateFileURL: URL {
        LaunchConfig.unpeelDir
            .appendingPathComponent("computer")
            .appendingPathComponent("daemon.json")
    }

    /// Whether a daemon for this home looks alive (socket present + the
    /// recorded pid identity-verifies). Cheap; used by Settings ▸ Computer.
    var isRunning: Bool {
        if process?.isRunning == true { return true }
        return Self.verifiedRecordedPid() != nil
    }

    /// Call once at app startup, and again whenever the policy inputs change
    /// (access picker, experimental flag) or grants may have (app became
    /// active after a System Settings visit).
    func startIfEnabled() {
        guard UnpeelFeatureFlags.computerUseAvailable else {
            // A prior development build may have left an owned daemon alive.
            // Release startup must reap it even when stale defaults still say
            // Computer Use is enabled.
            stop()
            return
        }
        if activationObserver == nil {
            activationObserver = NotificationCenter.default.addObserver(
                forName: NSApplication.didBecomeActiveNotification,
                object: nil,
                queue: .main
            ) { _ in
                Task { @MainActor in ComputerEngineManager.shared.sync() }
            }
        }
        if defaultsObserver == nil {
            // The experimental flag lives in AppDefaults; re-sync when it
            // (or anything else there) changes so toggling Computer use in
            // Settings starts/stops the daemon without an app restart.
            defaultsObserver = NotificationCenter.default.addObserver(
                forName: UserDefaults.didChangeNotification,
                object: AppDefaults.shared,
                queue: .main
            ) { _ in
                Task { @MainActor in ComputerEngineManager.shared.sync() }
            }
        }
        sync()
    }

    /// Reconcile the running child with policy and grant state.
    func sync() {
        if shouldRun {
            if process != nil {
                restartIfGrantsChanged()
                return
            }
            guard !gaveUp, pendingRestart == nil else { return }
            rapidFailures = 0
            launch()
        } else {
            gaveUp = false
            if process != nil || pendingRestart != nil || Self.verifiedRecordedPid() != nil {
                stop()
            }
        }
    }

    private var shouldRun: Bool {
        guard UnpeelFeatureFlags.computerUseAvailable else { return false }
        guard UnpeelFeatureFlags.isEnabled(.computerUse) else { return false }
        return Self.accessFromStateFile() != .off
    }

    /// `computer_default_access` straight from app-state.json — same
    /// absent→Ask / unknown→Off semantics as the Rust reader, and no store
    /// coupling so the manager can sync before the store finishes loading.
    private static func accessFromStateFile() -> ComputerAccess {
        let url = LaunchConfig.unpeelDir.appendingPathComponent("app-state.json")
        guard let data = try? Data(contentsOf: url),
              let object = try? JSONSerialization.jsonObject(with: data) as? [String: Any]
        else { return .ask }
        guard let raw = object["computer_default_access"] as? String else { return .ask }
        return ComputerAccess(rawValue: raw.lowercased()) ?? .off
    }

    // MARK: - Spawn / stop

    private func launch() {
        guard let engine = ComputerPermissions.resolveEngine() else {
            NSLog("[UnpeelNative] computer engine: cua-driver not found; daemon not started")
            return
        }

        reapOrphanedDaemons()

        let dir = LaunchConfig.unpeelDir.appendingPathComponent("computer")
        try? FileManager.default.createDirectory(at: dir, withIntermediateDirectories: true)
        // A stale socket file makes bind fail and the MCP side think the
        // daemon is up; the reap above guarantees no live owner remains.
        try? FileManager.default.removeItem(atPath: Self.socketPath)

        let child = Process()
        child.executableURL = URL(fileURLWithPath: engine)
        child.arguments = ["serve", "--embedded", "--socket", Self.socketPath]
        var env = ProcessInfo.processInfo.environment
        env["CUA_DRIVER_EMBEDDED"] = "1"
        env["CUA_DRIVER_HOST_BUNDLE_ID"] = Bundle.main.bundleIdentifier ?? "com.unpeel.native"
        // Unpeel owns approvals (Settings ▸ Computer gate + Ask alerts); the
        // driver's own runtime-approval layer would prompt into the void.
        env["CUA_DRIVER_PERMISSION_MODE"] = "unrestricted"
        env["CUA_DRIVER_DANGEROUSLY_BYPASS_APPROVALS"] = "1"
        child.environment = env
        child.standardInput = FileHandle.nullDevice
        child.standardOutput = FileHandle.nullDevice
        let stderr = Pipe()
        child.standardError = stderr
        stderr.fileHandleForReading.readabilityHandler = { handle in
            let data = handle.availableData
            guard !data.isEmpty,
                  let text = String(data: data, encoding: .utf8)?
                      .trimmingCharacters(in: .whitespacesAndNewlines),
                  !text.isEmpty else { return }
            NSLog("[UnpeelNative] computer engine: %@", text)
        }

        child.terminationHandler = { [weak self] proc in
            let code = proc.terminationStatus
            Task { @MainActor in self?.handleTermination(code: code) }
        }

        do {
            try child.run()
        } catch {
            NSLog("[UnpeelNative] computer engine: failed to launch: \(error)")
            return
        }

        process = child
        lastLaunchAt = Date()
        grantsAtLaunch = Self.currentGrants()
        Self.writeStateFile(pid: child.processIdentifier)
        NSLog(
            "[UnpeelNative] computer engine started (pid %d, socket %@)",
            child.processIdentifier, Self.socketPath
        )
    }

    private func handleTermination(code: Int32) {
        guard !stopping else { return }
        process = nil
        Self.removeStateFiles()
        let lifetime = Date().timeIntervalSince(lastLaunchAt)
        if lifetime < rapidExitWindow {
            rapidFailures += 1
        } else {
            rapidFailures = 0
        }
        guard shouldRun else { return }
        guard rapidFailures < maxRapidFailures else {
            NSLog("[UnpeelNative] computer engine: crash-looping (exit %d); giving up", code)
            gaveUp = true
            return
        }
        NSLog("[UnpeelNative] computer engine exited (%d); restarting", code)
        let work = DispatchWorkItem { [weak self] in
            Task { @MainActor in
                self?.pendingRestart = nil
                self?.sync()
            }
        }
        pendingRestart = work
        DispatchQueue.main.asyncAfter(deadline: .now() + restartDelay, execute: work)
    }

    /// Terminate the daemon (disable or app quit). Also reaps an orphan from
    /// a previous app instance when we have no Process handle.
    func stop() {
        pendingRestart?.cancel()
        pendingRestart = nil
        if let process {
            stopping = true
            (process.standardError as? Pipe)?.fileHandleForReading.readabilityHandler = nil
            if process.isRunning {
                process.terminate()
            }
            self.process = nil
            stopping = false
        } else if let pid = Self.verifiedRecordedPid() {
            kill(pid, SIGTERM)
        }
        grantsAtLaunch = nil
        Self.removeStateFiles()
        NSLog("[UnpeelNative] computer engine stopped")
    }

    /// TCC answers are cached per running process: a daemon spawned before a
    /// grant keeps answering "not granted" until relaunch.
    private func restartIfGrantsChanged() {
        let now = Self.currentGrants()
        guard let before = grantsAtLaunch, before != now else { return }
        NSLog("[UnpeelNative] computer engine: macOS grants changed; restarting daemon")
        stop()
        sync()
    }

    private static func currentGrants() -> (ax: Bool, sr: Bool) {
        (ComputerPermissions.accessibilityGranted(),
         ComputerPermissions.screenRecordingGranted())
    }

    // MARK: - Daemon state file (pid identity, same discipline as manifests)

    private static func writeStateFile(pid: Int32) {
        var object: [String: Any] = ["pid": pid, "socket": socketPath]
        if let started = UnpeelStore.processStartTimeMs(pid) {
            object["pid_started_at"] = started
        }
        if let data = try? JSONSerialization.data(
            withJSONObject: object, options: [.sortedKeys]
        ) {
            try? data.write(to: stateFileURL, options: .atomic)
        }
    }

    private static func removeStateFiles() {
        try? FileManager.default.removeItem(at: stateFileURL)
        try? FileManager.default.removeItem(atPath: socketPath)
    }

    /// The recorded daemon pid, but only when the kernel-reported start time
    /// matches what we wrote — pids recycle fast under agent load, and an
    /// unverified kill would hit an innocent process (session-manifest
    /// discipline).
    private static func verifiedRecordedPid() -> Int32? {
        guard let data = try? Data(contentsOf: stateFileURL),
              let object = try? JSONSerialization.jsonObject(with: data) as? [String: Any],
              let pid = (object["pid"] as? NSNumber)?.int32Value, pid > 1,
              let recorded = (object["pid_started_at"] as? NSNumber)?.uint64Value,
              let actual = UnpeelStore.processStartTimeMs(pid)
        else { return nil }
        let drift = actual > recorded ? actual - recorded : recorded - actual
        return drift <= 10_000 ? pid : nil
    }

    /// A hard-killed app leaves its daemon running (reparented to launchd);
    /// the next instance must not bind a second one on a fresh socket while
    /// the stale process lingers. Scoped to THIS home's state file — workspace
    /// instances own their homes' daemons.
    private func reapOrphanedDaemons() {
        guard process == nil, let pid = Self.verifiedRecordedPid() else { return }
        if kill(pid, SIGTERM) == 0 {
            NSLog("[UnpeelNative] computer engine: reaped orphaned daemon (pid %d)", pid)
        }
        Self.removeStateFiles()
    }
}
