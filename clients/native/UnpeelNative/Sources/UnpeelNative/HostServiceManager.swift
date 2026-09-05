//
//  HostServiceManager.swift
//  UnpeelNative
//
//  Starts the canonical Rust Host service embedded in `unpeel-host`. The
//  service intentionally outlives the window/app process: hosted Sessions
//  and Controller reachability are Host concerns, not renderer concerns.
//

import CUnpeelNativeBridge
import Foundation

@MainActor
final class HostServiceManager: ObservableObject {
    static let shared = HostServiceManager()

    /// What the UI may say about the bundled service. The app is always a
    /// client; this only decides whether a small non-modal status is shown.
    enum ServiceState: Equatable {
        /// Launch probe still inside `LocalHostClientFeature.launchDeadline`.
        case starting
        /// The worker answered host.sock (or the Local client connected).
        case live
        /// The launch probe expired or the Local connection failed after
        /// the deadline. Relaunches keep running; "Retry" forces one now.
        case unavailable(reason: String)
    }

    @Published private(set) var serviceState: ServiceState = .starting
    /// The reason the launch probe failed, if it did (for diagnostics).
    private(set) var launchProbeFailureReason: String?
    private var launchProbeSettled = false

    private(set) var launchPreparation: Task<Void, Never>?

    /// Keep retries behind reconciliation; otherwise a click during shutdown
    /// can launch or connect to the process we are about to replace.
    func prepareForLaunch(
        reconcile: @escaping @Sendable () -> Bool,
        launch: @escaping @MainActor (Bool) -> Void
    ) -> Task<Void, Never> {
        if let launchPreparation { return launchPreparation }
        noteLaunchProbeStarted()
        let task = Task { @MainActor in
            let restarted = await Task.detached(priority: .userInitiated, operation: reconcile).value
            launchPreparation = nil
            launch(restarted)
        }
        launchPreparation = task
        return task
    }

    private var lastLaunchAttemptAt: Date?
    private var platformAdapterHandle: unpeel_native_bridge_platform_adapter_handle_t?
    private weak var platformCallbackServer: HookServer?
    private static let relaunchCooldown: TimeInterval = 5

    init() {}

    /// `force` bypasses the relaunch cooldown: used right after the launch
    /// reconcile stopped a stale service so the replacement starts at once.
    func ensureStarted(now: Date = Date(), force: Bool = false) {
        guard launchPreparation == nil else { return }
        var environment = ProcessInfo.processInfo.environment
        // Snapshot/self-test app launches are disposable render harnesses,
        // not user Hosts. Never let one leave a background service behind.
        if environment.keys.contains(where: {
            $0.hasPrefix("UNPEEL_TEST_") || $0.hasPrefix("UNPEEL_SNAPSHOT")
        }) {
            return
        }
        guard force || Self.shouldAttemptLaunch(
            now: now,
            lastAttemptAt: lastLaunchAttemptAt,
            cooldown: Self.relaunchCooldown
        ) else { return }
        lastLaunchAttemptAt = now

        let binary = LaunchConfig.hostBinary
        guard FileManager.default.isExecutableFile(atPath: binary) else {
            NSLog("[UnpeelNative] Host service: unpeel-host not found at %@", binary)
            return
        }

        // The default app and every registered workspace all start the SAME
        // machine service. It discovers the shared workspace registry and
        // supervises one isolated worker per home. An unregistered home is a
        // dev-blank/test-style private instance, so preserve its UNPEEL_HOME
        // and start only that one scoped worker.
        if UnpeelWorkspaceContext.isDefaultInstance
            || UnpeelWorkspaceContext.currentWorkspace() != nil
        {
            environment.removeValue(forKey: "UNPEEL_HOME")
        }

        let process = Process()
        process.executableURL = URL(fileURLWithPath: binary)
        process.arguments = ["__serve__"]
        process.environment = environment
        process.standardInput = FileHandle.nullDevice
        process.standardOutput = FileHandle.nullDevice
        process.standardError = FileHandle.nullDevice
        do {
            try process.run()
            // Do not retain or terminate it. The machine/service lease picks
            // one winner when multiple app instances launch concurrently;
            // losing children exit immediately.
            NSLog("[UnpeelNative] Host service launch requested (pid %d)", process.processIdentifier)
        } catch {
            NSLog("[UnpeelNative] Host service failed to launch: %@", error.localizedDescription)
        }
    }

    /// Register the native-only operations this app can perform for its local
    /// workspace. The Rust bridge keeps one connection-scoped registration
    /// alive across worker restarts; the worker is still the Host/protocol
    /// authority and withdraws the capability as soon as this process leaves.
    func startPlatformAdapter(on server: HookServer) {
        guard server.port > 0 else { return }
        if platformAdapterHandle != nil {
            // A replacement HookServer has a different port and must never
            // inherit the old listener's bearer registration. Tear down the
            // reconnecting bridge first, then mint/register one matching
            // token for the new callback endpoint. Repeated calls for the
            // same live listener remain idempotent.
            guard platformCallbackServer !== server else { return }
            stopPlatformAdapter()
        }
        let token = Self.platformAdapterToken()
        server.platformAdapterToken = token
        struct Config: Encodable {
            let unpeelHome: String
            let instanceID: String
            let callbackPort: UInt16
            let callbackToken: String
            let capabilities: [String]
        }
        let config: Data
        do {
            config = try JSONEncoder().encode(Config(
                unpeelHome: LaunchConfig.unpeelDir.standardizedFileURL.path,
                instanceID: "native-\(ProcessInfo.processInfo.processIdentifier)-\(UUID().uuidString)",
                callbackPort: server.port,
                callbackToken: token,
                capabilities: [
                    "approval.present",
                    "app.open-in-editor",
                    "artifact.thumbnail",
                    "computer.status",
                    "controller.transport.host-owned",
                    "link.entitlement.refresh",
                    "mobile.e2e-key.reconcile",
                    "notification.deliver",
                    "overlay.snapshot",
                    "overlay.project-color.set",
                    "push.register",
                    "relay.credentials.recover",
                    "session.notify_when_done.set",
                ]
            ))
        } catch {
            server.platformAdapterToken = nil
            NSLog("[UnpeelNative] Host platform adapter config failed: %@", error.localizedDescription)
            return
        }

        var handle: unpeel_native_bridge_platform_adapter_handle_t = 0
        var outputPointer: UnsafeMutablePointer<UInt8>?
        var outputLength = 0
        let result = config.withUnsafeBytes { bytes in
            unpeel_native_bridge_platform_adapter_start(
                bytes.bindMemory(to: UInt8.self).baseAddress,
                bytes.count,
                &handle,
                &outputPointer,
                &outputLength
            )
        }
        let output = Self.takeBridgeOutput(outputPointer, length: outputLength)
        guard result == UNPEEL_NATIVE_BRIDGE_OK, handle != 0 else {
            server.platformAdapterToken = nil
            let message = String(data: output, encoding: .utf8) ?? "unknown bridge error"
            NSLog("[UnpeelNative] Host platform adapter failed: %@", message)
            return
        }
        platformAdapterHandle = handle
        platformCallbackServer = server
    }

    // MARK: - Service state (launch probe + Local connection)

    func noteLaunchProbeStarted() {
        launchProbeSettled = false
        if serviceState != .live { serviceState = .starting }
    }

    func noteLaunchProbeSucceeded() {
        launchProbeSettled = true
        launchProbeFailureReason = nil
        if serviceState != .live { serviceState = .live }
    }

    func noteLaunchProbeFailed(reason: String) {
        launchProbeSettled = true
        launchProbeFailureReason = reason
        if serviceState != .live {
            serviceState = .unavailable(reason: reason)
        }
    }

    /// The Local client connected: the service is proven live regardless of
    /// what the launch probe concluded.
    func noteLocalConnectionEstablished() {
        launchProbeFailureReason = nil
        if serviceState != .live { serviceState = .live }
    }

    /// The Local client lost or could not make its connection. Before the
    /// launch probe settles this stays "starting" so a slow first boot never
    /// flashes an error; afterwards it is reported and a relaunch requested.
    func noteLocalConnectionFailed(reason: String) {
        ensureStarted()
        guard launchProbeSettled else { return }
        if case .live = serviceState {
            serviceState = .unavailable(reason: reason)
        } else if case .starting = serviceState {
            serviceState = .unavailable(reason: reason)
        }
    }

    /// User-driven retry from the status banner: bypass the cooldown once.
    func retryNow() {
        LaunchTrace.append("native-app Host service: user retry")
        serviceState = .starting
        launchProbeSettled = true
        ensureStarted(force: true)
    }

    func stopPlatformAdapter() {
        platformCallbackServer?.platformAdapterToken = nil
        platformCallbackServer = nil
        guard let handle = platformAdapterHandle else { return }
        platformAdapterHandle = nil
        var outputPointer: UnsafeMutablePointer<UInt8>?
        var outputLength = 0
        let result = unpeel_native_bridge_platform_adapter_stop(
            handle,
            &outputPointer,
            &outputLength
        )
        let output = Self.takeBridgeOutput(outputPointer, length: outputLength)
        if result != UNPEEL_NATIVE_BRIDGE_OK {
            let message = String(data: output, encoding: .utf8) ?? "unknown bridge error"
            NSLog("[UnpeelNative] Host platform adapter stop failed: %@", message)
        }
    }

    nonisolated static func platformAdapterToken() -> String {
        (UUID().uuidString + UUID().uuidString)
            .replacingOccurrences(of: "-", with: "")
            .lowercased()
    }

    private nonisolated static func takeBridgeOutput(
        _ pointer: UnsafeMutablePointer<UInt8>?,
        length: Int
    ) -> Data {
        guard let pointer, length > 0 else { return Data() }
        let data = Data(bytes: pointer, count: length)
        unpeel_native_bridge_free(pointer, length)
        return data
    }

    /// Pure retry policy used by tests. A failed worker connection may call
    /// `ensureStarted` repeatedly; the machine/workspace leases make another
    /// launch safe, while this bound avoids a child-process storm.
    nonisolated static func shouldAttemptLaunch(
        now: Date,
        lastAttemptAt: Date?,
        cooldown: TimeInterval
    ) -> Bool {
        guard let lastAttemptAt else { return true }
        return now.timeIntervalSince(lastAttemptAt) >= max(0, cooldown)
    }
}
