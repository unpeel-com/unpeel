//
//  HostServiceIdentity.swift
//  UnpeelNative
//
//  Detects an app/Host-service version skew and restarts the stale service
//  once per launch. After a Sparkle update the bundled `unpeel-host` service
//  keeps running the OLD binary image while the new app expects the new
//  contract (the 2026-09-01 approval 404s). The worker publishes its
//  executable, version, and build stamp in serve.json (0.4.0+); a service
//  published by an older binary carries none of them and counts as skewed.
//

import Darwin
import Foundation

enum HostServiceIdentity {
    /// The subset of `serve.json` / `host-service.json` this check reads.
    struct Record: Decodable, Equatable {
        let pid: Int32
        let startedAtUnixMs: UInt64
        let executable: String?
        let hostVersion: String?
        let buildId: String?
        let workspaces: [Workspace]?

        struct Workspace: Decodable, Equatable {
            let home: String
            let pid: Int32?
        }
    }

    /// This app's own bundled Host identity.
    struct Own: Equatable, Sendable {
        let executable: String
        let version: String
        let buildId: String?
    }

    enum Decision: Equatable {
        /// Nothing to do (no service, a matching one, or a foreign same-version
        /// service such as `unpeel serve install`'s launchd job).
        case keep(reason: String)
        case restart(pid: Int32, reason: String)
    }

    /// Pure skew policy. A different version restarts in either direction
    /// (an older service from a previous install, or a newer one left behind
    /// by a rolled-back app). The same version only restarts when it is OUR
    /// executable path with a different build stamp — the in-place Sparkle
    /// replacement — so a CLI-installed service of the same version keeps
    /// running. A record without identity fields was written by a pre-0.4.0
    /// binary and is always stale. At most one restart per app launch.
    nonisolated static func decide(
        record: Record?,
        own: Own,
        restartedThisLaunch: Bool
    ) -> Decision {
        guard let record else { return .keep(reason: "no Host service record") }
        guard !restartedThisLaunch else {
            return .keep(reason: "already restarted the Host service once this launch")
        }
        guard let version = record.hostVersion, let recordBuild = record.buildId else {
            return .restart(
                pid: record.pid,
                reason: "service record has no identity fields (pre-0.4.0 binary)"
            )
        }
        if version != own.version {
            return .restart(
                pid: record.pid,
                reason: "service is \(version), app bundles \(own.version)"
            )
        }
        let sameExecutable = record.executable.map {
            URL(fileURLWithPath: $0).standardizedFileURL.path
                == URL(fileURLWithPath: own.executable).standardizedFileURL.path
        } ?? false
        if sameExecutable, let ownBuild = own.buildId, ownBuild != recordBuild {
            return .restart(
                pid: record.pid,
                reason: "service runs a replaced image of \(own.executable) (build \(recordBuild) vs \(ownBuild))"
            )
        }
        return .keep(reason: sameExecutable ? "service matches the bundled Host" : "foreign service of the same version")
    }

    /// Same stamp `unpeel_core::session_host::current_host_build_id` computes
    /// for the running binary: `<mtime secs>.<nanos 9 digits>:<size>`.
    nonisolated static func buildID(forExecutableAt path: String) -> String? {
        var st = stat()
        guard stat(path, &st) == 0 else { return nil }
        return String(
            format: "%ld.%09ld:%lld",
            st.st_mtimespec.tv_sec,
            st.st_mtimespec.tv_nsec,
            Int64(st.st_size)
        )
    }

    nonisolated static func readRecord(at url: URL) -> Record? {
        guard let data = try? Data(contentsOf: url) else { return nil }
        return try? JSONDecoder().decode(Record.self, from: data)
    }

    /// The process at `pid` is the recorded service: alive, started within
    /// tolerance of the record's own timestamp (the service writes `now` at
    /// startup, so a few seconds separate exec from publish), and is an
    /// unpeel-host image. Anything else is a recycled pid and is never signaled.
    ///
    /// The image test accepts either the executable path or the kernel's
    /// process name. After a Sparkle update the stale service still runs the
    /// image Sparkle staged under its Caches directory, which Sparkle deletes
    /// once the install lands — `proc_pidpath` then has no path to return
    /// and the service would never be recognized, so it would keep serving
    /// the old build (seen 2026-09-03: a 0.4.1 worker outlived two updates).
    /// The start-time match is the identity proof; the name check only
    /// keeps a recycled pid from being signaled.
    nonisolated static func processMatchesRecord(pid: Int32, startedAtUnixMs: UInt64) -> Bool {
        guard pid > 1, kill(pid, 0) == 0,
              let actual = UnpeelStore.processStartTimeMs(pid)
        else { return false }
        let drift = actual > startedAtUnixMs ? actual - startedAtUnixMs : startedAtUnixMs - actual
        guard drift <= 30_000 else { return false }
        var buffer = [CChar](repeating: 0, count: Int(4 * MAXPATHLEN))
        let length = proc_pidpath(pid, &buffer, UInt32(buffer.count))
        let path = length > 0 ? String(cString: buffer) : nil
        return isUnpeelHostImage(path: path, processName: processName(pid))
    }

    /// Pure image test shared by `processMatchesRecord`: the executable path
    /// when the kernel can still resolve one, otherwise the process name,
    /// which survives deletion of the image file.
    nonisolated static func isUnpeelHostImage(path: String?, processName: String?) -> Bool {
        if let path, !path.isEmpty {
            return path.hasSuffix("/unpeel-host")
        }
        return processName == "unpeel-host"
    }

    /// The kernel's name for the process (`pbi_name`, falling back to the
    /// truncated `pbi_comm`), which does not depend on the image file.
    nonisolated static func processName(_ pid: Int32) -> String? {
        var info = proc_bsdinfo()
        let size = Int32(MemoryLayout<proc_bsdinfo>.stride)
        guard proc_pidinfo(pid, PROC_PIDTBSDINFO, 0, &info, size) == size else { return nil }
        let name = withUnsafePointer(to: &info.pbi_name) {
            $0.withMemoryRebound(to: CChar.self, capacity: Int(2 * MAXCOMLEN)) { String(cString: $0) }
        }
        if !name.isEmpty { return name }
        let comm = withUnsafePointer(to: &info.pbi_comm) {
            $0.withMemoryRebound(to: CChar.self, capacity: Int(MAXCOMLEN)) { String(cString: $0) }
        }
        return comm.isEmpty ? nil : comm
    }

    /// Terminate one identity-verified process and wait briefly for it to go.
    nonisolated static func terminate(pid: Int32, startedAtUnixMs: UInt64, wait: TimeInterval = 4) -> Bool {
        guard processMatchesRecord(pid: pid, startedAtUnixMs: startedAtUnixMs) else { return false }
        guard kill(pid, SIGTERM) == 0 else { return false }
        let deadline = Date().addingTimeInterval(wait)
        while Date() < deadline {
            if kill(pid, 0) != 0 { return true }
            Thread.sleep(forTimeInterval: 0.05)
        }
        return kill(pid, 0) != 0
    }

    /// Stop whatever serves `home`: the machine supervisor when it manages
    /// this home's worker (it stops its workers cleanly), otherwise the scoped
    /// worker itself. Returns true when something was signaled.
    nonisolated static func stopService(servingHome home: URL, realHome: URL) -> Bool {
        let worker = readRecord(at: home.appendingPathComponent("serve.json"))
        let supervisor = readRecord(at: realHome.appendingPathComponent("host-service.json"))
        let normalizedHome = home.standardizedFileURL.resolvingSymlinksInPath().path
        var stopped = false
        if let supervisor,
           supervisor.workspaces?.contains(where: {
               URL(fileURLWithPath: $0.home).standardizedFileURL.resolvingSymlinksInPath().path == normalizedHome
                   && (worker == nil || $0.pid == worker?.pid)
           }) == true {
            stopped = terminate(pid: supervisor.pid, startedAtUnixMs: supervisor.startedAtUnixMs)
        }
        if let worker, kill(worker.pid, 0) == 0 {
            stopped = terminate(pid: worker.pid, startedAtUnixMs: worker.startedAtUnixMs) || stopped
        }
        return stopped
    }

    private static let launchLock = NSLock()
    private nonisolated(unsafe) static var restartedThisLaunch = false

    /// Launch-time reconcile for the app's own workspace. Returns whether the
    /// stale service was restarted (the caller respawns it immediately).
    nonisolated static func reconcileAtLaunch(
        home: URL,
        realHome: URL,
        own: Own,
        log: (String) -> Void
    ) -> Bool {
        launchLock.lock()
        defer { launchLock.unlock() }
        let record = readRecord(at: home.appendingPathComponent("serve.json"))
        switch decide(record: record, own: own, restartedThisLaunch: restartedThisLaunch) {
        case .keep(let reason):
            if record != nil { log("Host service kept: \(reason)") }
            return false
        case .restart(_, let reason):
            restartedThisLaunch = true
            let stopped = stopService(servingHome: home, realHome: realHome)
            log("Host service restart (\(reason)): \(stopped ? "stopped the stale service" : "no live service to stop")")
            return true
        }
    }

    /// Test hook: forget the once-per-launch guard.
    nonisolated static func resetLaunchGuardForTesting() {
        launchLock.lock()
        defer { launchLock.unlock() }
        restartedThisLaunch = false
    }
}
