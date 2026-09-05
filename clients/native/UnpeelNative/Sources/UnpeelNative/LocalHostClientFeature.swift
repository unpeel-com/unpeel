import Foundation

/// The app's own Local scope is always a Controller of the canonical Rust
/// workspace worker (`unpeel-host __serve__`). There is no in-app Host any
/// more: the launch policy starts (or restarts a stale) bundled service and
/// then keeps the already-scanned disk view visible while the worker comes
/// up. A service that never answers `host.sock` is reported through
/// `HostServiceManager.serviceState` (a small non-modal "Host service
/// unavailable — Retry" state) and retried with the bounded relaunch; the
/// app never fails open to a second Host implementation.
enum LocalHostClientFeature {
    /// Bounded wait for the bundled service to answer host.sock at launch
    /// before the UI reports it as unavailable. Everything keeps working as
    /// a client during that window; only the status changes afterwards.
    static let launchDeadline: TimeInterval = 5

    enum LaunchResolution: Equatable {
        case client
        /// The service did not answer within the deadline. The app stays a
        /// client (fail closed) and surfaces the reason; it never hosts.
        case unavailable(reason: String)
    }

    /// Pure launch policy used by tests: poll `probe` until it answers or
    /// `deadline` passes. Both outcomes leave the app a client.
    nonisolated static func resolve(
        probe: () -> Bool,
        deadline: TimeInterval,
        now: () -> Date = Date.init,
        sleep: (TimeInterval) -> Void = { Thread.sleep(forTimeInterval: $0) }
    ) -> LaunchResolution {
        let start = now()
        var attempts = 0
        while true {
            attempts += 1
            if probe() { return .client }
            if now().timeIntervalSince(start) >= deadline {
                return .unavailable(
                    reason: "Host service did not answer host.sock within \(Int(deadline))s (\(attempts) probes)"
                )
            }
            sleep(0.1)
        }
    }

    /// Start this launch's Host service. Called once from
    /// `applicationDidFinishLaunching`, before the store connects:
    /// 1. restart a stale service (version/build skew) at most once,
    /// 2. start the bundled service,
    /// 3. probe host.sock off the main thread for up to `launchDeadline` and
    ///    publish the outcome as `HostServiceManager.serviceState`.
    /// The UI never waits on this: the disk seed stays visible and the store's
    /// own connection loop keeps retrying through `HostServiceManager`.
    @MainActor
    @discardableResult
    static func resolveForLaunch(
        home: URL = LaunchConfig.unpeelDir,
        realHome: URL = LaunchConfig.realUnpeelDir
    ) -> Task<Void, Never> {
        let log: @Sendable (String) -> Void = { message in
            NSLog("[UnpeelNative] %@", message)
            LaunchTrace.append("native-app \(message)")
        }
        let own = HostServiceIdentity.Own(
            executable: LaunchConfig.hostBinary,
            version: Bundle.main.object(forInfoDictionaryKey: "CFBundleShortVersionString") as? String ?? "",
            buildId: HostServiceIdentity.buildID(forExecutableAt: LaunchConfig.hostBinary)
        )
        return HostServiceManager.shared.prepareForLaunch {
            HostServiceIdentity.reconcileAtLaunch(
                home: home, realHome: realHome, own: own, log: log
            )
        } launch: { restarted in
            HostServiceManager.shared.ensureStarted(force: restarted)
            let homePath = home.standardizedFileURL.path
            Task.detached(priority: .userInitiated) {
                let resolution = resolve(
                    probe: { LocalHostControl.probeBlocking(home: homePath) },
                    deadline: launchDeadline
                )
                log("Local scope: Host service probe: \(resolution)")
                await MainActor.run {
                    switch resolution {
                    case .client:
                        HostServiceManager.shared.noteLaunchProbeSucceeded()
                    case .unavailable(let reason):
                        HostServiceManager.shared.noteLaunchProbeFailed(reason: reason)
                    }
                }
            }
        }
    }

    /// Advertised on every native loopback response. The worker probes this
    /// independently of the platform-adapter socket so a transient adapter
    /// reconnect can never make Direct/Link ownership bounce back to Swift.
    static let controllerOwnerHeaderValue = "serve"
}

/// Launch diagnostics land in `~/.unpeel/hooks/trace.log` next to the hook
/// and worker lines so one file tells the story of a launch.
enum LaunchTrace {
    nonisolated static func append(_ line: String, home: URL = LaunchConfig.unpeelDir) {
        let url = home.appendingPathComponent("hooks").appendingPathComponent("trace.log")
        let stamped = "\(UInt64(Date().timeIntervalSince1970 * 1000)) \(line)\n"
        guard let data = stamped.data(using: .utf8) else { return }
        try? FileManager.default.createDirectory(
            at: url.deletingLastPathComponent(), withIntermediateDirectories: true
        )
        if let handle = try? FileHandle(forWritingTo: url) {
            defer { try? handle.close() }
            _ = try? handle.seekToEnd()
            try? handle.write(contentsOf: data)
        } else {
            try? data.write(to: url)
        }
    }
}
