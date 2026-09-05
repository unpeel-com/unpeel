//
//  LaunchConfig.swift
//  UnpeelNative
//
//  Resolves binary paths and per-session attach commands.
//  No Ghostty types in here — this is plain app configuration.
//

import Foundation

enum LaunchConfig {
    /// Environment variable that overrides the attach binary
    /// (kept from the Phase 0 spike: UNPEEL_ATTACH_CMD). When set, it is
    /// used verbatim as the command prefix and the session id is appended.
    static let attachCommandEnvVar = "UNPEEL_ATTACH_CMD"

    /// Environment variable that overrides the unpeel-host binary path.
    static let hostCommandEnvVar = "UNPEEL_HOST_CMD"

    /// Repo root for an app assembled under `<repo>/clients/native/dist`.
    ///
    /// Derive this from the bundle location instead of `#filePath`: the latter
    /// embeds the release builder's absolute checkout path in every shipped
    /// binary. Installed release bundles always resolve their bundled helpers
    /// first, so this fallback is only meaningful for the repository's dev
    /// bundle.
    static let repoRoot: String = {
        var url = Bundle.main.bundleURL.standardizedFileURL
        for _ in 0..<4 { url.deleteLastPathComponent() }
        return url.path
    }()

    /// Path to the Rust attach client (renders inside the Ghostty surface).
    /// The `UNPEEL_ATTACH_CMD` override is handled in `attachCommand`; here we
    /// resolve bundled binary (packaged app) → workspace target dirs (dev
    /// builds, release preferred), matching `hostBinary`.
    static var attachBinary: String {
        if let bundled = Bundle.main.url(forAuxiliaryExecutable: "unpeel-attach"),
           FileManager.default.isExecutableFile(atPath: bundled.path) {
            return bundled.path
        }
        let candidates = [
            "\(repoRoot)/crates/unpeel-attach/target/release/unpeel-attach",
            "\(repoRoot)/crates/unpeel-attach/target/debug/unpeel-attach",
        ]
        for candidate in candidates where FileManager.default.isExecutableFile(atPath: candidate) {
            return candidate
        }
        return candidates[1]
    }

    /// Path to the standalone session host (`crates/unpeel-host`).
    /// Resolution order: env override → bundled binary (packaged app) →
    /// workspace target dirs (dev builds, release preferred).
    static var hostBinary: String {
        if let override = ProcessInfo.processInfo.environment[hostCommandEnvVar],
           !override.trimmingCharacters(in: .whitespaces).isEmpty {
            return override
        }
        if let bundled = Bundle.main.url(forAuxiliaryExecutable: "unpeel-host"),
           FileManager.default.isExecutableFile(atPath: bundled.path) {
            return bundled.path
        }
        let candidates = [
            "\(repoRoot)/crates/target/release/unpeel-host",
            "\(repoRoot)/crates/target/debug/unpeel-host",
        ]
        for candidate in candidates where FileManager.default.isExecutableFile(atPath: candidate) {
            return candidate
        }
        return candidates[0]
    }

    /// The command a Ghostty surface runs to render a hosted session.
    /// Every branch below uses `direct:`, which makes Ghostty exec the
    /// binary instead of wrapping it in `login(1)`. That wrapper would print
    /// the "Last login"/"You have mail" banner above every attach AND keep a
    /// 7–8 MiB `login` process alive per pane for the life of the surface;
    /// `direct:` skips it entirely, so neither concern applies.
    ///
    /// `sessionsDir` scopes the attach to ANOTHER local workspace's
    /// `app-sessions` directory (workspaces selected in this window through
    /// the loopback gateway). The workspace home is on this disk, so the
    /// surface runs the very same client as Local scope: a persistent
    /// `session.sock` stream with on-disk tail replay — never the paged
    /// remote transport. `nil` keeps this instance's own home.
    static func attachCommand(sessionID: String, sessionsDir: URL? = nil) -> String {
        if let override = ProcessInfo.processInfo.environment[attachCommandEnvVar],
           !override.trimmingCharacters(in: .whitespaces).isEmpty {
            if let sessionsDir {
                return "\(override) --sessions-dir \(sessionsDir.path) \(sessionID)"
            }
            return "\(override) \(sessionID)"
        }
        if let sessionsDir, sessionsDir != appSessionsDir {
            let home = sessionsDir.deletingLastPathComponent().path
            return "direct:/usr/bin/env UNPEEL_HOME=\(home) \(attachBinary)"
                + " --sessions-dir \(sessionsDir.path) \(sessionID)"
        }
        // When running against a dev/blank state dir, the in-surface attach
        // client must resolve the same UNPEEL_HOME as the app — but libghostty
        // does not reliably forward the app's environment to the surface
        // command, so the host writes the session into the blank dir while a
        // bare attach would look in the real ~/.unpeel and find nothing.
        // Pass the dir as an explicit argv flag: argv survives any env
        // scrubbing the surface command can go through, and it works with
        // attach binaries that predate UNPEEL_HOME support. The env(1)
        // injection stays as well for anything attach spawns.
        // Production has no UNPEEL_HOME and keeps the byte-for-byte
        // original command.
        if let home = ProcessInfo.processInfo.environment["UNPEEL_HOME"],
           !home.trimmingCharacters(in: .whitespaces).isEmpty {
            let sessionsDir = unpeelDir
                .appendingPathComponent("app-sessions", isDirectory: true).path
            return "direct:/usr/bin/env UNPEEL_HOME=\(home) \(attachBinary)"
                + " --sessions-dir \(sessionsDir) \(sessionID)"
        }
        return "direct:\(attachBinary) \(sessionID)"
    }

    /// ~/.unpeel — or the directory named by `UNPEEL_HOME` when set, so a dev
    /// instance can run against a throwaway state dir (fresh first-run state,
    /// isolated sessions/projects/license) without touching the real ~/.unpeel.
    /// Spawned `unpeel-host` processes inherit the env var, so the Rust side
    /// (`app_paths::unpeel_home`) resolves to the same dir. Note: this is
    /// separate from $HOME because `homeDirectoryForCurrentUser` ignores $HOME.
    /// Computed once: the env can't change after launch, and
    /// `ProcessInfo.environment` materializes the whole env dictionary on
    /// every call — as a computed var this getter dominated the rescan
    /// profile (one call per session dir per marker check).
    static let unpeelDir: URL = {
        if let override = ProcessInfo.processInfo.environment["UNPEEL_HOME"],
           !override.trimmingCharacters(in: .whitespaces).isEmpty {
            return URL(fileURLWithPath: (override as NSString).expandingTildeInPath, isDirectory: true)
        }
        return FileManager.default.homeDirectoryForCurrentUser
            .appendingPathComponent(".unpeel")
    }()

    /// The real per-user Unpeel root, ignoring `UNPEEL_HOME` — where the
    /// machine-wide Host service keeps `host-service.json` and its lease
    /// (`unpeel_core::app_paths::real_unpeel_home`).
    static let realUnpeelDir: URL = FileManager.default.homeDirectoryForCurrentUser
        .appendingPathComponent(".unpeel")

    static var appStateFile: URL {
        unpeelDir.appendingPathComponent("app-state.json")
    }

    static let appSessionsDir: URL = unpeelDir.appendingPathComponent("app-sessions")

    static var activityStateFile: URL {
        unpeelDir.appendingPathComponent("activity-state.json")
    }

    /// Append-only session-activity history feed (see ActivityLog.swift).
    static var activityLogFile: URL {
        unpeelDir.appendingPathComponent("activity-log.jsonl")
    }

}

/// The UserDefaults the app reads/writes for every native overlay (projects,
/// presets, favorites, CLI availability, titles, pins, theme, …).
///
/// Normally `.standard`. But `.standard` is keyed by the bundle identifier, so
/// without this a dev/blank instance launched with `UNPEEL_HOME` would still
/// inherit the real instance's overlays — `UNPEEL_HOME` only isolates on-disk
/// `~/.unpeel` files, not the defaults domain. When `UNPEEL_HOME` is set we use
/// a suite derived from that path so the blank instance is fully isolated.
enum AppDefaults {
    // UserDefaults is internally thread-safe but not `Sendable`; this matches
    // how `UserDefaults.standard` is used freely across the app.
    nonisolated(unsafe) static let shared: UserDefaults = {
        let env = ProcessInfo.processInfo.environment
        return suite(forUnpeelHome: env["UNPEEL_HOME"])
    }()

    /// The defaults suite an instance launched with `UNPEEL_HOME=home` uses —
    /// the same derivation `shared` applies to this process's own env. Pass
    /// the home path exactly as the instance receives it (the workspace
    /// launcher passes the registry record's `home` verbatim): the suite name
    /// hashes the raw string, not a normalized path. nil/empty = the default
    /// instance's `.standard`. Lets one workspace read another's overlays
    /// (e.g. the sidebar workspace selector showing each workspace's tint).
    nonisolated static func suite(forUnpeelHome home: String?) -> UserDefaults {
        if let home,
           !home.trimmingCharacters(in: .whitespaces).isEmpty {
            let suite = "com.unpeel.devhome." + String(stableHash(home), radix: 36)
            if let defaults = UserDefaults(suiteName: suite) {
                return defaults
            }
        }
        return .standard
    }

    /// Deterministic FNV-1a hash — Swift's `String.hashValue` is salted per
    /// process, which would change the suite name every launch and lose
    /// persistence for a fixed `UNPEEL_HOME`.
    private static func stableHash(_ string: String) -> UInt64 {
        var hash: UInt64 = 0xcbf29ce484222325
        for byte in string.utf8 {
            hash = (hash ^ UInt64(byte)) &* 0x100000001b3
        }
        return hash
    }
}
