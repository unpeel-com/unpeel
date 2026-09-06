//
//  HostServiceAgent.swift
//  UnpeelNative
//
//  Starts the bundled Host service through launchd instead of forking it.
//
//  Why: macOS stamps every process with its parent's coalition at fork, and
//  `setsid` does not leave it. Force Quit (Dock, Force Quit window, Activity
//  Monitor) terminates the app's whole jetsam coalition, so a service the app
//  forked — and every worker, session host, and PTY core forked under it —
//  died with the app (2026-09-06: the only terminal created since that app
//  instance had started the service was killed 44 ms after the Force Quit).
//  A launchd job is a child of launchd with its own coalition, so the service
//  chain survives any termination of the app. Normal Quit never mattered.
//
//  The agent is the same per-user LaunchAgent shape `unpeel serve install`
//  writes (packaging/service/com.unpeel.serve.plist), under the app's own
//  label so a CLI-installed unit and the app's never rewrite each other's
//  file. Two loaded units start two services; the machine lease picks one
//  winner and the loser exits, which is why the app's unit must not use
//  KeepAlive — launchd would respawn the loser every ThrottleInterval. The app
//  kickstarts the job whenever it cannot reach the service, so a crashed
//  service comes back on the next connection attempt.
//

import Darwin
import Foundation

enum HostServiceAgent {
    static let releaseLabel = "com.unpeel.native.serve"
    /// Dev builds share the release bundle id but must never re-point the
    /// user's real unit at `dist/Unpeel.app`; they get their own label.
    static let developmentLabel = "com.unpeel.native.dev.serve"
    static let bundleIdentifier = "com.unpeel.native"
    /// `UNPEEL_NATIVE_SERVICE_LAUNCHER=direct` restores the pre-2026-09-06
    /// fork for diagnostics. Anything else (or unset) uses launchd.
    static let launcherOverrideEnvVar = "UNPEEL_NATIVE_SERVICE_LAUNCHER"

    struct CommandResult: Equatable {
        let status: Int32
        let output: String
    }

    typealias Launchctl = @Sendable ([String]) -> CommandResult

    enum Outcome: Equatable {
        /// The job is loaded and was asked to run. `rewrote` says the plist
        /// on disk changed (first install, moved bundle, or a new argv).
        case running(label: String, rewrote: Bool)
        /// launchd refused; the caller falls back to forking the service.
        case failed(String)
    }

    nonisolated static var isDevelopmentBuild: Bool {
        Bundle.main.object(forInfoDictionaryKey: "UnpeelDevelopmentBuild") as? Bool == true
    }

    nonisolated static func label(developmentBuild: Bool) -> String {
        developmentBuild ? developmentLabel : releaseLabel
    }

    nonisolated static var currentLabel: String {
        label(developmentBuild: isDevelopmentBuild)
    }

    nonisolated static func usesDirectLaunch(environment: [String: String]) -> Bool {
        environment[launcherOverrideEnvVar]?
            .trimmingCharacters(in: .whitespacesAndNewlines)
            .lowercased() == "direct"
    }

    /// `~/Library/LaunchAgents` for the real user home (`UNPEEL_HOME` never
    /// moves it: launchd domains are per user, not per workspace).
    nonisolated static func launchAgentsDirectory(
        userHome: URL = FileManager.default.homeDirectoryForCurrentUser
    ) -> URL {
        userHome
            .appendingPathComponent("Library", isDirectory: true)
            .appendingPathComponent("LaunchAgents", isDirectory: true)
    }

    nonisolated static func plistURL(label: String, agentsDirectory: URL) -> URL {
        agentsDirectory.appendingPathComponent("\(label).plist", isDirectory: false)
    }

    /// The unit body. Deterministic so an unchanged install is a byte-equal
    /// file and never re-bootstrapped.
    nonisolated static func renderPlist(label: String, hostBinary: String) throws -> Data {
        let unit: [String: Any] = [
            "Label": label,
            "ProgramArguments": [hostBinary, "__serve__"],
            "RunAtLoad": true,
            // No KeepAlive: see the header. `ensureRunning` kickstarts.
            "KeepAlive": false,
            "ProcessType": "Interactive",
            // Login Items groups the background item under the app.
            "AssociatedBundleIdentifiers": [bundleIdentifier],
        ]
        return try PropertyListSerialization.data(
            fromPropertyList: unit,
            format: .xml,
            options: 0
        )
    }

    /// Install (or refresh) the unit and make sure launchd runs it. Pure
    /// apart from the injected `launchctl` and the plist write, so tests
    /// drive it with a temp directory and a fake tool.
    nonisolated static func ensureRunning(
        label: String,
        hostBinary: String,
        agentsDirectory: URL,
        uid: uid_t,
        launchctl: Launchctl
    ) -> Outcome {
        let plist = plistURL(label: label, agentsDirectory: agentsDirectory)
        let desired: Data
        do {
            desired = try renderPlist(label: label, hostBinary: hostBinary)
        } catch {
            return .failed("could not render \(plist.lastPathComponent): \(error.localizedDescription)")
        }
        let existing = try? Data(contentsOf: plist)
        let rewrote = existing != desired
        if rewrote {
            do {
                try FileManager.default.createDirectory(
                    at: agentsDirectory,
                    withIntermediateDirectories: true
                )
                try desired.write(to: plist, options: .atomic)
            } catch {
                return .failed("could not write \(plist.path): \(error.localizedDescription)")
            }
        }

        let domain = "gui/\(uid)"
        let target = "\(domain)/\(label)"
        if rewrote {
            // A loaded job keeps the argv it was bootstrapped with; unload it
            // so the rewritten file takes effect. Fails harmlessly when it
            // was never loaded.
            _ = launchctl(["bootout", target])
        }
        let bootstrap = launchctl(["bootstrap", domain, plist.path])
        if bootstrap.status != 0 {
            // Already loaded is the common case on every launch after the
            // first; anything else must show up in `print`.
            let loaded = launchctl(["print", target])
            guard loaded.status == 0 else {
                return .failed(
                    "launchctl bootstrap \(target) failed (\(bootstrap.status)): \(bootstrap.output.trimmed)"
                )
            }
        }
        let kickstart = launchctl(["kickstart", target])
        guard kickstart.status == 0 else {
            return .failed(
                "launchctl kickstart \(target) failed (\(kickstart.status)): \(kickstart.output.trimmed)"
            )
        }
        return .running(label: label, rewrote: rewrote)
    }

    /// The real tool. Synchronous; every call returns in milliseconds.
    nonisolated static func runLaunchctl(_ arguments: [String]) -> CommandResult {
        let process = Process()
        process.executableURL = URL(fileURLWithPath: "/bin/launchctl")
        process.arguments = arguments
        let pipe = Pipe()
        process.standardOutput = pipe
        process.standardError = pipe
        process.standardInput = FileHandle.nullDevice
        do {
            try process.run()
        } catch {
            return CommandResult(status: -1, output: error.localizedDescription)
        }
        let data = pipe.fileHandleForReading.readDataToEndOfFile()
        process.waitUntilExit()
        return CommandResult(
            status: process.terminationStatus,
            output: String(data: data, encoding: .utf8) ?? ""
        )
    }
}

private extension String {
    var trimmed: String { trimmingCharacters(in: .whitespacesAndNewlines) }
}
