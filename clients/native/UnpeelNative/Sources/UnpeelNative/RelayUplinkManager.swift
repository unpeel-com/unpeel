//
//  RelayUplinkManager.swift
//  UnpeelNative
//
//  The app's client-side half of Unpeel Link. The Relay uplink itself (the
//  Host WebSocket to apps/relay, tunneled requests, output streams) is owned
//  by the workspace worker (`unpeel serve`). What stays here is what only
//  the app can do: the shared durable Link authority record (activation,
//  deactivation, service rejections), the Host-bound entitlement fetch the
//  worker delegates through the `link.entitlement.refresh` platform
//  callback (the license key never leaves Keychain/this process), and APNs
//  push through the Link service. Entitlements are cached in
//  ~/.unpeel/mobile/relay-entitlement.json.
//

import AppKit
import Darwin
import Foundation
import UnpeelShared

/// Uplink diagnostics land in the shared `~/.unpeel/hooks/trace.log`
/// (timestamped) as well as NSLog: unified logging proved unqueryable for
/// this app during the 2026-08-30 "no uplink on 5G" outage, leaving the one
/// component whose reconnects bounce every phone completely unobservable.
private func relayTrace(_ message: String) {
    NSLog("[relay-uplink] \(message)")
    let url = LaunchConfig.unpeelDir
        .appendingPathComponent("hooks")
        .appendingPathComponent("trace.log")
    let line = "\(UInt64(Date().timeIntervalSince1970 * 1000)) relay-uplink-app \(message)\n"
    guard let data = line.data(using: .utf8) else { return }
    if let handle = try? FileHandle(forWritingTo: url) {
        defer { try? handle.close() }
        _ = try? handle.seekToEnd()
        try? handle.write(contentsOf: data)
    } else {
        try? data.write(to: url)
    }
}

enum RelayConfig {
    /// The retired global "Access away from home" toggle's key. New builds
    /// gate the uplink purely on per-device enrollment, but the key stays
    /// alive for downgrade compatibility: `migrateLegacyRelayPreference`
    /// folds a stored `false` into the enrollment list once, and enrolling a
    /// device writes `true` back so an older build (which still reads this
    /// key) doesn't silently cut relay access for enrolled phones.
    static let enabledDefaultsKey = "unpeel.native.remoteAccessEnabled"
    /// One-shot marker for `migrateLegacyRelayPreference`.
    static let enrollmentMigratedDefaultsKey = "unpeel.native.linkEnrollmentMigrated"
    /// Hidden override for dev (`ws://127.0.0.1:8787` against `wrangler dev`).
    static let urlOverrideDefaultsKey = "unpeel.native.relayURL"
    private static let productionURL = URL(string: "wss://relay.unpeel.com")!

    static var relayURL: URL {
        if let raw = AppDefaults.shared.string(forKey: urlOverrideDefaultsKey),
           let url = URL(string: raw.trimmingCharacters(in: .whitespacesAndNewlines)),
           url.scheme == "ws" || url.scheme == "wss" {
            return url
        }
        return productionURL
    }
}

/// Durable Link authority shared by the native Host and the headless Rust
/// Host. A cached entitlement is a 30-day bearer, so deleting the cache alone
/// is not a sufficient revocation primitive: an unlink can fail, or another
/// frontend can have already read it. The marker lives outside `mobile/`, is
/// written before cache removal, and is serialized with Rust through the same
/// `link-license.lock` flock.
enum LinkSuppressionReason: String, Codable {
    case userDisabled = "user_disabled"
    case authorizationRejected = "authorization_rejected"
    /// `/api/activate` and the local key commit succeeded, but a fresh relay
    /// entitlement has not committed yet. Cached authority stays blocked;
    /// automatic refresh is safe across process restart.
    case activationPending = "activation_pending"
}

struct LinkSuppressionRecord: Codable, Equatable {
    let version: Int
    let generation: String
    let reason: LinkSuppressionReason
    let disabledAt: Int64

    enum CodingKeys: String, CodingKey {
        case version, generation, reason
        case disabledAt = "disabled_at"
    }
}

struct LinkCachedEntitlement: Codable, Equatable {
    let entitlement: String
    let expiresAt: Int64
    let macID: String
}

enum LinkAuthorityStore {
    struct LocalState: Equatable {
        let suppression: LinkSuppressionRecord?
        let cached: LinkCachedEntitlement?
    }

    struct SuppressionOutcome: Equatable {
        let record: LinkSuppressionRecord
        /// A committed marker already fails closed. Keep an unlink diagnostic
        /// for logs/tests without turning a safe deactivation into failure.
        let cacheRemovalError: String?
    }

    private static func suppressionURL(home: URL) -> URL {
        home.appendingPathComponent("link-disabled.json")
    }

    private static func lockURL(home: URL) -> URL {
        home.appendingPathComponent("link-license.lock")
    }

    private static func cacheURL(home: URL) -> URL {
        home.appendingPathComponent("mobile")
            .appendingPathComponent("relay-entitlement.json")
    }

    private static func posixError(_ operation: String) -> NSError {
        let code = errno
        return NSError(
            domain: NSPOSIXErrorDomain,
            code: Int(code),
            userInfo: [NSLocalizedDescriptionKey: "\(operation): \(String(cString: strerror(code)))"]
        )
    }

    private static func withLock<T>(home: URL, _ body: () throws -> T) throws -> T {
        try FileManager.default.createDirectory(at: home, withIntermediateDirectories: true)
        let descriptor = open(lockURL(home: home).path, O_CREAT | O_RDWR | O_CLOEXEC, 0o600)
        guard descriptor >= 0 else { throw posixError("could not open Link authority lock") }
        defer { _ = close(descriptor) }
        guard fchmod(descriptor, 0o600) == 0 else {
            throw posixError("could not secure Link authority lock")
        }
        guard flock(descriptor, LOCK_EX) == 0 else {
            throw posixError("could not acquire Link authority lock")
        }
        defer { _ = flock(descriptor, LOCK_UN) }
        return try body()
    }

    private static func fileKind(at url: URL) throws -> mode_t? {
        var value = stat()
        guard lstat(url.path, &value) == 0 else {
            if errno == ENOENT { return nil }
            throw posixError("could not inspect \(url.lastPathComponent)")
        }
        return value.st_mode & mode_t(S_IFMT)
    }

    private static func readSuppressionUnlocked(home: URL) throws -> LinkSuppressionRecord? {
        let url = suppressionURL(home: home)
        guard let kind = try fileKind(at: url) else { return nil }
        guard kind == mode_t(S_IFREG) else {
            throw NSError(
                domain: "UnpeelLinkAuthority",
                code: 1,
                userInfo: [NSLocalizedDescriptionKey: "Link disable marker is not a regular file"]
            )
        }
        let record = try JSONDecoder().decode(LinkSuppressionRecord.self, from: Data(contentsOf: url))
        guard record.version == 1,
              !record.generation.isEmpty
        else {
            throw NSError(
                domain: "UnpeelLinkAuthority",
                code: 2,
                userInfo: [NSLocalizedDescriptionKey: "Link disable marker is invalid"]
            )
        }
        return record
    }

    private static func writePrivateAtomically(_ data: Data, to url: URL) throws {
        let directory = url.deletingLastPathComponent()
        try FileManager.default.createDirectory(at: directory, withIntermediateDirectories: true)
        let temporary = directory.appendingPathComponent(".\(url.lastPathComponent).\(UUID().uuidString).tmp")
        defer { try? FileManager.default.removeItem(at: temporary) }
        guard FileManager.default.createFile(
            atPath: temporary.path,
            contents: data,
            attributes: [.posixPermissions: 0o600]
        ) else {
            throw NSError(
                domain: "UnpeelLinkAuthority",
                code: 3,
                userInfo: [NSLocalizedDescriptionKey: "could not create \(url.lastPathComponent)"]
            )
        }
        let handle = try FileHandle(forWritingTo: temporary)
        try handle.synchronize()
        try handle.close()
        guard rename(temporary.path, url.path) == 0 else {
            throw posixError("could not publish \(url.lastPathComponent)")
        }
    }

    private static func removeCacheUnlocked(home: URL) -> String? {
        let url = cacheURL(home: home)
        guard unlink(url.path) != 0 else { return nil }
        if errno == ENOENT { return nil }
        return posixError("could not remove cached Link entitlement").localizedDescription
    }

    static func localState(home: URL, macID: String) throws -> LocalState {
        try withLock(home: home) {
            let suppression = try readSuppressionUnlocked(home: home)
            var cached: LinkCachedEntitlement?
            if suppression == nil,
               try fileKind(at: cacheURL(home: home)) == mode_t(S_IFREG),
               let data = try? Data(contentsOf: cacheURL(home: home)) {
                cached = try? JSONDecoder().decode(LinkCachedEntitlement.self, from: data)
                if cached?.macID != macID { cached = nil }
            }
            return LocalState(suppression: suppression, cached: cached)
        }
    }

    static func suppression(home: URL) throws -> LinkSuppressionRecord? {
        try withLock(home: home) { try readSuppressionUnlocked(home: home) }
    }

    static func suppress(home: URL, reason: LinkSuppressionReason) throws -> SuppressionOutcome {
        try withLock(home: home) {
            // A late transport/service rejection may strengthen an active or
            // pending state, but it must never weaken an explicit user off.
            if reason == .authorizationRejected,
               let current = try readSuppressionUnlocked(home: home),
               current.reason == .userDisabled {
                return SuppressionOutcome(
                    record: current,
                    cacheRemovalError: removeCacheUnlocked(home: home)
                )
            }
            let record = LinkSuppressionRecord(
                version: 1,
                generation: UUID().uuidString.lowercased(),
                reason: reason,
                disabledAt: Int64(Date().timeIntervalSince1970)
            )
            try writePrivateAtomically(try JSONEncoder().encode(record), to: suppressionURL(home: home))
            return SuppressionOutcome(
                record: record,
                cacheRemovalError: removeCacheUnlocked(home: home)
            )
        }
    }

    /// Convert only the authority generation observed before `/api/activate`
    /// began. A deactivation that happened while the request was in flight
    /// writes a different generation and therefore wins. Even a fresh
    /// activation gets a pending marker: a legacy/pre-marker cache must never
    /// authorize a newly activated (possibly different) key.
    static func markActivationPending(
        home: URL,
        expectedSuppressionGeneration: String?
    ) throws -> String? {
        try withLock(home: home) {
            let current = try readSuppressionUnlocked(home: home)
            guard current?.generation == expectedSuppressionGeneration else {
                throw NSError(
                    domain: "UnpeelLinkAuthority",
                    code: 5,
                    userInfo: [NSLocalizedDescriptionKey: "Link was disabled while activating"]
                )
            }
            let pending = LinkSuppressionRecord(
                version: current?.version ?? 1,
                generation: current?.generation ?? UUID().uuidString.lowercased(),
                reason: .activationPending,
                disabledAt: current?.disabledAt ?? Int64(Date().timeIntervalSince1970)
            )
            try writePrivateAtomically(
                try JSONEncoder().encode(pending),
                to: suppressionURL(home: home)
            )
            if let warning = removeCacheUnlocked(home: home) {
                // The marker already makes the retained bearer unusable. A
                // later fresh commit will replace it; keep the diagnostic for
                // operators without weakening the durable deny.
                relayTrace("activation cache invalidation failed: \(warning)")
            }
            return pending.generation
        }
    }

    /// Commit a fresh entitlement only if no newer deactivation/rejection won
    /// while the network request was in flight. Publishing the cache before
    /// clearing the exact marker keeps every crash point fail-closed.
    static func commit(
        _ entitlement: LinkCachedEntitlement,
        expectedSuppressionGeneration: String?,
        home: URL
    ) throws {
        try withLock(home: home) {
            let current = try readSuppressionUnlocked(home: home)
            guard current?.generation == expectedSuppressionGeneration else {
                throw NSError(
                    domain: "UnpeelLinkAuthority",
                    code: 4,
                    userInfo: [NSLocalizedDescriptionKey: "Link authority changed while authorizing"]
                )
            }
            try writePrivateAtomically(
                try JSONEncoder().encode(entitlement),
                to: cacheURL(home: home)
            )
            if current != nil {
                guard unlink(suppressionURL(home: home).path) == 0 else {
                    throw posixError("could not clear Link disable marker")
                }
            }
        }
    }
}

/// Client-side half of Unpeel Link that stays in the app after the Swift
/// Host retirement: the shared durable authority record (activation,
/// deactivation, service rejections), the Host-bound entitlement fetch the
/// worker delegates through `link.entitlement.refresh`, and APNs push
/// through the Link service. The Relay uplink itself (the Host socket,
/// tunneled requests, output streams) is owned by `unpeel serve`.
@MainActor
final class RelayUplinkManager: ObservableObject {
    static let shared = RelayUplinkManager()

    enum Status: Equatable {
        case off
        case needsLicense
        case error(String)

        var label: String {
            switch self {
            case .off: return "Host-owned"
            case .needsLicense: return "Requires an active license"
            case .error(let message): return message
            }
        }
    }

    @Published private(set) var status: Status = .off

    enum PushDiagnostic: Equatable {
        case neverAttempted
        case delivered
        case failed(String)

        var label: String {
            switch self {
            case .neverAttempted: return "No push attempted yet"
            case .delivered: return "Delivered to APNs"
            case .failed(let message): return message
            }
        }
    }

    @Published private(set) var lastPushDiagnostic: PushDiagnostic = .neverAttempted
    @Published private(set) var lastPushAttemptAt: Date?

    /// Covers the current process even when persisting the durable marker
    /// itself failed. A failed local deactivation is reported to the user,
    /// but it must still refuse every later entitlement use immediately.
    private var authoritySuppressedInMemory = false

    private init() {}

    /// Re-read the shared authority after a Settings change. The worker
    /// owns the live uplink; this only refreshes what the app may show.
    func refresh() {
        _ = stopForSharedSuppressionIfNeeded()
    }

    /// Immediate, restart-safe local shutdown. The network seat release runs
    /// later; it is never allowed to keep an established Relay socket alive.
    func deactivateLocalAuthority() -> String? {
        authoritySuppressedInMemory = true
        do {
            let outcome = try LinkAuthorityStore.suppress(
                home: LaunchConfig.unpeelDir,
                reason: .userDisabled
            )
            if let warning = outcome.cacheRemovalError {
                relayTrace("cache removal after durable deactivation failed: \(warning)")
            }
            status = .needsLicense
            return nil
        } catch {
            status = .error("Could not persist Link deactivation")
            return error.localizedDescription
        }
    }

    /// A server revocation is also durable authority. Unlike a user disable,
    /// it may recover automatically after the stored key becomes valid again,
    /// but never by reusing the rejected cached bearer.

    /// A server revocation is also durable authority. Unlike a user disable,
    /// it may recover automatically after the stored key becomes valid again,
    /// but never by reusing the rejected cached bearer.
    func rejectLocalAuthority() -> String? {
        persistAuthorizationRejection(message: "Unpeel Link authorization rejected")
    }

    /// Every authoritative service rejection takes the same local-first path:
    /// close the live socket, publish the shared deny marker, then invalidate
    /// the cached bearer. A late rejection must not weaken a user-disabled
    /// marker written while its request was in flight.

    /// Every authoritative service rejection takes the same local-first path:
    /// close the live socket, publish the shared deny marker, then invalidate
    /// the cached bearer. A late rejection must not weaken a user-disabled
    /// marker written while its request was in flight.
    private func persistAuthorizationRejection(message: String) -> String? {
        do {
            let outcome = try LinkAuthorityStore.suppress(
                home: LaunchConfig.unpeelDir,
                reason: .authorizationRejected
            )
            if let warning = outcome.cacheRemovalError {
                relayTrace("cache removal after authorization rejection failed: \(warning)")
            }
            authoritySuppressedInMemory = outcome.record.reason == .userDisabled
            status = authoritySuppressedInMemory ? .needsLicense : .error(message)
            return nil
        } catch {
            authoritySuppressedInMemory = true
            status = .error("Could not persist Link authorization rejection")
            return error.localizedDescription
        }
    }

    /// Snapshot the durable generation before `/api/activate` starts. The
    /// finishing commit must still see this exact generation so a concurrent
    /// deactivation (native or TUI) always wins.

    /// Snapshot the durable generation before `/api/activate` starts. The
    /// finishing commit must still see this exact generation so a concurrent
    /// deactivation (native or TUI) always wins.
    func activationSuppressionGeneration() throws -> String? {
        try LinkAuthorityStore.suppression(home: LaunchConfig.unpeelDir)?.generation
    }

    /// Called only after `/api/activate` accepted a key and the Keychain
    /// commit succeeded. Make that recovery permission durable before any
    /// entitlement request; the marker itself still blocks cached access.

    /// Called only after `/api/activate` accepted a key and the Keychain
    /// commit succeeded. Make that recovery permission durable before any
    /// entitlement request; the marker itself still blocks cached access.
    func resumeAfterExplicitActivation(
        expectedSuppressionGeneration: String?
    ) -> String? {
        do {
            _ = try LinkAuthorityStore.markActivationPending(
                home: LaunchConfig.unpeelDir,
                expectedSuppressionGeneration: expectedSuppressionGeneration
            )
            authoritySuppressedInMemory = false
            return nil
        } catch {
            authoritySuppressedInMemory = true
            status = .error("Link changed while activation was finishing")
            return error.localizedDescription
        }
    }

    nonisolated static func isAuthorizationRejection(_ statusCode: Int?) -> Bool {
        statusCode == 401 || statusCode == 403
    }

    /// Return true after stopping for a durable suppression or an unreadable
    /// authority file. Rejection/activation-pending may immediately seek a
    /// fresh entitlement; an explicit user disable remains off.
    private func stopForSharedSuppressionIfNeeded() -> Bool {
        do {
            guard let suppression = try LinkAuthorityStore.suppression(
                home: LaunchConfig.unpeelDir
            ) else { return false }
                authoritySuppressedInMemory = suppression.reason == .userDisabled
            if authoritySuppressedInMemory {
                status = .needsLicense
            }
            return true
        } catch {
                authoritySuppressedInMemory = true
            status = .error("Link authority state is unreadable")
            return true
        }
    }

    /// Filesystem state is the authority and may be changed by the TUI, so an
    /// in-process NotificationCenter observer is only the fast path. Poll the
    /// small registration set while connected; any membership/scope/token
    /// change tears down the uplink generation, which makes the Relay close
    /// every old client and prevents quiet output subscriptions surviving a
    /// cross-process revocation.

    /// Keychain-safe half of the canonical Host's Link refresh. The worker
    /// supplies only its public Host id; this app reads the legacy license key
    /// from Keychain, performs the service request, and commits through the
    /// existing shared authority transaction. The bearer and license key are
    /// never returned over the platform callback.
    func refreshPlatformEntitlement(macID: String) async -> Bool {
        guard let entitlement = await currentEntitlement(macID: macID) else {
            return false
        }
        let now = Int64(Date().timeIntervalSince1970)
        if let local = try? LinkAuthorityStore.localState(
            home: LaunchConfig.unpeelDir,
            macID: macID
        ), local.suppression == nil,
           let cached = local.cached,
           cached.entitlement == entitlement,
           cached.expiresAt > now {
            return true
        }

        // The local Relay development bearer historically bypasses the
        // entitlement cache because Swift presented it directly to its own
        // socket. A client-only Host needs the same value in the shared cache;
        // this branch is compiled/runtime-gated exactly like the old bypass.
        if LicenseConfig.developmentBuildLicenseBypassEnabled,
           let devToken = AppDefaults.shared.string(
               forKey: "unpeel.native.relayDevToken"
           )?.trimmingCharacters(in: .whitespacesAndNewlines),
           !devToken.isEmpty,
           devToken == entitlement {
            do {
                try LinkAuthorityStore.commit(
                    LinkCachedEntitlement(
                        entitlement: entitlement,
                        expiresAt: now + 30 * 24 * 3600,
                        macID: macID
                    ),
                    expectedSuppressionGeneration: nil,
                    home: LaunchConfig.unpeelDir
                )
                return true
            } catch {
                relayTrace("native Host dev entitlement commit failed: \(error)")
            }
        }
        return false
    }

    private struct EntitlementResponse: Decodable {
        let entitlement: String
        let expiresAt: Int64

        enum CodingKeys: String, CodingKey {
            case entitlement
            case expiresAt = "expires_at"
        }
    }

    /// Cached entitlement while >7 days of validity remain; otherwise a
    /// fresh one from unpeel.com using the stored license key. Nil (with
    /// status set) when there's no license or the server refuses.

    /// Cached entitlement while >7 days of validity remain; otherwise a
    /// fresh one from unpeel.com using the stored license key. Nil (with
    /// status set) when there's no license or the server refuses.
    private func currentEntitlement(macID: String) async -> String? {
        guard !authoritySuppressedInMemory else { return nil }
        let now = Int64(Date().timeIntervalSince1970)
        let local: LinkAuthorityStore.LocalState
        do {
            local = try LinkAuthorityStore.localState(
                home: LaunchConfig.unpeelDir,
                macID: macID
            )
        } catch {
            // Unreadable/malformed durable deny is deny, never absence.
            authoritySuppressedInMemory = true
            status = .error("Link authority state is unreadable")
            return nil
        }
        if let suppression = local.suppression,
           suppression.reason == .userDisabled {
            authoritySuppressedInMemory = true
            status = .needsLicense
            return nil
        }

        // LOCAL-DEV ONLY: a dev token (default `unpeel.native.relayDevToken`)
        // is presented verbatim as the entitlement, skipping the unpeel.com
        // fetch — pairs with the relay Worker's DEV_ENTITLEMENT_BYPASS so a
        // dev Mac with a dev-signed license can run the relay locally. Unset
        // in real builds, so production still fetches a signed entitlement.
        if LicenseConfig.developmentBuildLicenseBypassEnabled,
           local.suppression == nil,
           let devToken = AppDefaults.shared.string(forKey: "unpeel.native.relayDevToken"),
           !devToken.trimmingCharacters(in: .whitespaces).isEmpty {
            return devToken
        }
        guard let licenseKey = LicenseManager.shared.currentLicenseKey else {
            status = .needsLicense
            return nil
        }
        // A cached bearer is never authority on its own. Keychain deletion,
        // revocation, or an unlicensed restart must deny it even if durable
        // cache invalidation encountered a filesystem failure.
        if let cached = local.cached,
           cached.expiresAt > now + 7 * 24 * 3600 {
            return cached.entitlement
        }
        var request = URLRequest(
            url: LicenseConfig.apiBaseURL.appendingPathComponent("api/remote/entitlement")
        )
        request.httpMethod = "POST"
        request.timeoutInterval = 15
        request.setValue("application/json", forHTTPHeaderField: "Content-Type")
        request.httpBody = try? JSONSerialization.data(withJSONObject: [
            "key": licenseKey,
            "mac_id": macID,
            "device_id": LicenseManager.deviceID,
        ])
        do {
            let (data, response) = try await URLSession.shared.data(for: request)
            guard let http = response as? HTTPURLResponse else { throw URLError(.badServerResponse) }
            guard http.statusCode == 200 else {
                if [400, 401, 402, 403, 404, 409, 410, 422].contains(http.statusCode) {
                    _ = persistAuthorizationRejection(
                        message: http.statusCode == 402
                            ? "Remote access requires Unpeel Link"
                            : "Could not authorize remote access (HTTP \(http.statusCode))"
                    )
                    }
                return nil
            }
            let issued = try JSONDecoder().decode(EntitlementResponse.self, from: data)
            guard issued.expiresAt > now else { throw URLError(.badServerResponse) }
            // Deactivation/key replacement may have won while URLSession was
            // suspended. Never let that late response publish authority.
            guard LicenseManager.shared.currentLicenseKey == licenseKey else { return nil }
            let cached = LinkCachedEntitlement(
                entitlement: issued.entitlement,
                expiresAt: issued.expiresAt,
                macID: macID
            )
            try LinkAuthorityStore.commit(
                cached,
                expectedSuppressionGeneration: local.suppression?.generation,
                home: LaunchConfig.unpeelDir
            )
            authoritySuppressedInMemory = false
            return issued.entitlement
        } catch {
            status = .error("Could not reach unpeel.com for remote access")
            return nil
        }
    }

    struct PushResult {
        let ok: Bool
        /// APNs `reason` on failure (e.g. `BadDeviceToken`, `Unregistered`),
        /// so the caller can prune a dead token.
        let reason: String?
    }

    static func pushFailureLabel(reason: String?) -> String {
        switch reason {
        case "remote-disabled": return "Unpeel Link is turned off"
        case "no-mac": return "The Host has no Link identity"
        case "no-entitlement": return "Link entitlement unavailable"
        case "forbidden": return "Link entitlement rejected"
        case "bad-url": return "Invalid Link service URL"
        case "network": return "Could not reach Unpeel Link"
        case "apns-not-configured": return "Link push is not configured"
        case "BadDeviceToken", "Unregistered": return "APNs rejected the device token"
        case "too many pushes": return "Link push rate limit reached"
        case "bad-token", "bad-message", "bad-metadata", "message-too-large":
            return "Push request was rejected"
        case .some(let reason): return "Push failed: \(reason)"
        case .none: return "Link returned an invalid push response"
        }
    }

    private func recordPushResult(_ result: PushResult) -> PushResult {
        lastPushAttemptAt = Date()
        lastPushDiagnostic = result.ok
            ? .delivered
            : .failed(Self.pushFailureLabel(reason: result.reason))
        return result
    }

    /// Forward one alert to APNs through the relay's `/v1/push/<macID>`
    /// (entitlement-gated — the same paid boundary as the streaming uplink).
    /// Independent of the WS socket, so it works even when streaming is idle.
    /// The relay owns the APNs key and signs the provider JWT; the Mac only
    /// supplies the device token + text.

    /// Forward one alert to APNs through the relay's `/v1/push/<macID>`
    /// (entitlement-gated — the same paid boundary as the streaming uplink).
    /// Independent of the WS socket, so it works even when streaming is idle.
    /// The relay owns the APNs key and signs the provider JWT; the Mac only
    /// supplies the device token + text.
    func sendPush(
        apnsToken: String,
        environment: String,
        title: String,
        body: String,
        sessionID: String,
        kind: String,
        macID explicitMacID: String? = nil
    ) async -> PushResult {
        // No global relay toggle anymore: enrollment is per-device, enforced
        // where push targets are collected (`MobilePairingStore.pushTargets`
        // skips Direct-only devices — pushes ride the Link service too).
        guard UnpeelFeatureFlags.mobileRemoteControlEnabled else {
            return recordPushResult(PushResult(ok: false, reason: "remote-disabled"))
        }
        guard let macID = explicitMacID else {
            return recordPushResult(PushResult(ok: false, reason: "no-mac"))
        }
        guard let entitlement = await currentEntitlement(macID: macID) else {
            return recordPushResult(PushResult(ok: false, reason: "no-entitlement"))
        }
        // A TUI/native deactivation may have landed while entitlement lookup
        // awaited the network. Recheck before presenting the bearer to Push.
        guard !stopForSharedSuppressionIfNeeded() else {
            return recordPushResult(PushResult(ok: false, reason: "no-entitlement"))
        }
        var components = URLComponents(url: RelayConfig.relayURL, resolvingAgainstBaseURL: false)
        components?.scheme = RelayConfig.relayURL.scheme == "wss" ? "https" : "http"
        guard let httpBase = components?.url else {
            return recordPushResult(PushResult(ok: false, reason: "bad-url"))
        }
        let base = httpBase.absoluteString.trimmingCharacters(in: CharacterSet(charactersIn: "/"))
        guard let url = URL(string: "\(base)/v1/push/\(macID)") else {
            return recordPushResult(PushResult(ok: false, reason: "bad-url"))
        }
        var request = URLRequest(url: url)
        request.httpMethod = "POST"
        request.timeoutInterval = 15
        request.setValue("Bearer \(entitlement)", forHTTPHeaderField: "Authorization")
        request.setValue("application/json", forHTTPHeaderField: "Content-Type")
        request.httpBody = try? JSONSerialization.data(withJSONObject: [
            "apnsToken": apnsToken,
            "environment": environment,
            "title": title,
            "body": body,
            "sessionId": sessionID,
            "kind": kind,
        ])
        do {
            let (data, response) = try await URLSession.shared.data(for: request)
            let http = response as? HTTPURLResponse
            let parsed = (try? JSONSerialization.jsonObject(with: data)) as? [String: Any]
            let ok = http?.statusCode == 200 && (parsed?["ok"] as? Bool == true)
            if Self.isAuthorizationRejection(http?.statusCode) {
                _ = persistAuthorizationRejection(message: "Link entitlement rejected")
                return recordPushResult(PushResult(ok: false, reason: "forbidden"))
            }
            let reason = parsed?["reason"] as? String
                ?? parsed?["error"] as? String
                ?? http.map { "http-\($0.statusCode)" }
            return recordPushResult(PushResult(ok: ok, reason: reason))
        } catch {
            return recordPushResult(PushResult(ok: false, reason: "network"))
        }
    }
}
