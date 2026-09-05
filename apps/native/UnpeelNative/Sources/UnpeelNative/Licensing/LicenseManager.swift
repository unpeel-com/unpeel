//
//  LicenseManager.swift
//  UnpeelNative
//
//  Client side of the Stripe → license → activation flow (see
//  docs/feature/licensing.md and the issuing Worker in apps/website).
//
//  A Unpeel Pro license is an Ed25519-signed token:
//
//      CLRTY-<payloadB64url>.<signatureB64url>
//
//  The app is free — a license is never required for local use. An active
//  license unlocks **Unpeel Pro** (Unpeel Remote relay access, iPhone
//  pairing, workspaces). The signature is verified OFFLINE here with CryptoKit
//  against the embedded public key. A light ONLINE step (activate / periodic
//  validate) binds the device, enforces the seat count, and lets a lapsed or
//  refunded key be revoked — Pro keeps working through network outages
//  thanks to a long offline grace window.
//
//  The payload carries no expiry. Validity is the signature plus the
//  server's `status` (active | revoked); subscription lapse surfaces as
//  `revoked` on /api/validate.
//
//  Key format MUST stay byte-compatible with apps/website/app/lib/license.ts.
//

import Combine
import CryptoKit
import Foundation
import IOKit

enum LicenseConfig {
    /// Raw 32-byte Ed25519 public key, base64. Produced by
    /// `bun run keygen` in apps/website.
    static let bundledPublicKeyBase64 = "6RfwwHUhth8Ji7T7p/QbDOQjeN9Zrk1S34Hk85cpg54="

    private static let productionAPIBaseURL = URL(string: "https://unpeel.com")!
    private static let publicKeyOverrideEnvKey = "UNPEEL_LICENSE_PUBLIC_KEY"
    private static let apiBaseURLOverrideEnvKey = "UNPEEL_LICENSE_API_BASE_URL"
    static let developmentBuildInfoPlistKey = "UnpeelDevelopmentBuild"

    static var publicKeyBase64: String {
        publicKeyBase64(environment: ProcessInfo.processInfo.environment)
    }

    static func publicKeyBase64(environment: [String: String]) -> String {
        guard let override = environment[publicKeyOverrideEnvKey]?.trimmingCharacters(
            in: .whitespacesAndNewlines
        ), !override.isEmpty else {
            return bundledPublicKeyBase64
        }
        return override
    }

    /// Where the activation API lives (the apps/website Worker).
    static var apiBaseURL: URL {
        apiBaseURL(environment: ProcessInfo.processInfo.environment)
    }

    static func apiBaseURL(environment: [String: String]) -> URL {
        guard let override = environment[apiBaseURLOverrideEnvKey]?.trimmingCharacters(
            in: .whitespacesAndNewlines
        ), !override.isEmpty,
            let url = URL(string: override),
            url.scheme != nil,
            url.host != nil
        else {
            return productionAPIBaseURL
        }
        return url
    }

    static var developmentBuildLicenseBypassEnabled: Bool {
        developmentBuildLicenseBypassEnabled(infoDictionary: Bundle.main.infoDictionary)
    }

    static func developmentBuildLicenseBypassEnabled(infoDictionary: [String: Any]?) -> Bool {
        guard let rawValue = infoDictionary?[developmentBuildInfoPlistKey] else {
            return false
        }
        if let enabled = rawValue as? Bool {
            return enabled
        }
        if let enabled = rawValue as? NSNumber {
            return enabled.boolValue
        }
        if let enabled = rawValue as? String {
            switch enabled.trimmingCharacters(in: .whitespacesAndNewlines).lowercased() {
            case "1", "true", "yes":
                return true
            default:
                return false
            }
        }
        return false
    }

    /// Re-check the server for revocation roughly this often.
    static let recheckInterval: TimeInterval = 7 * 24 * 60 * 60

    /// Keep a license active offline for this long after the last successful
    /// validate, so a flaky network never locks out a paying user.
    static let offlineGrace: TimeInterval = 30 * 24 * 60 * 60

    static let keyPrefix = "CLRTY-"
}

/// The decoded, signature-bearing license payload (mirror of LicensePayload
/// in license.ts).
struct LicensePayload: Codable, Equatable {
    let v: Int
    let id: String
    let email: String
    let plan: String
    let seats: Int
    let iat: Int
}

enum LicenseState: Equatable {
    case unlicensed
    case active(LicensePayload)
    case revoked(LicensePayload)

    var isActive: Bool { if case .active = self { return true } else { return false } }
}

@MainActor
final class LicenseManager: ObservableObject {
    static let shared = LicenseManager()

    enum ValidationResponse: Equatable {
        case active
        case revoked
        case invalid
    }

    @Published private(set) var state: LicenseState = .unlicensed
    /// Human-readable result of the last activate attempt (shown in the panel).
    @Published var lastError: String?
    @Published var working = false
    private var storedLicenseKey: String?
    /// Invalidates responses from async activation/revalidation work after a
    /// newer local license operation has taken authority.
    private var licenseStateGeneration = 0

    /// True once the Keychain-stored license (if any) has been applied. The
    /// Sparkle updater waits for this before starting, so a licensed user's
    /// first scheduled check doesn't fire against a still-`unlicensed` state
    /// and silently abort until the next 24h check interval.
    private(set) var initialLoadComplete = false
    static let initialLoadNotification = Notification.Name("unpeel.license.initialLoadComplete")

    /// Unpeel Pro is unlocked by an active license (or the dev-build bypass).
    /// The app itself is free and never gated; Pro covers the device-linking
    /// features — Unpeel Remote relay access, iPhone pairing, and workspaces.
    var isPro: Bool {
        LicenseConfig.developmentBuildLicenseBypassEnabled || state.isActive
    }

    /// The stored key while the license is active — used by the Unpeel
    /// Remote uplink to fetch relay entitlements from unpeel.com.
    var currentLicenseKey: String? {
        guard state.isActive else { return nil }
        return storedLicenseKey
    }

    var updateAuthorizationHeaders: [String: String]? {
        guard state.isActive,
              let stored = storedLicenseKey,
              verify(stored) != nil
        else { return nil }
        return [
            "X-Unpeel-License": stored,
            "X-Unpeel-Device-ID": Self.deviceID
        ]
    }

    private let publicKey: Curve25519.Signing.PublicKey?
    private let session = URLSession(configuration: .ephemeral)
    private let lastValidatedKey = "license.lastValidatedAt"
    private let revokedLicenseIDKey = "license.revokedID"

    private init() {
        if let data = Data(base64Encoded: LicenseConfig.publicKeyBase64),
           let key = try? Curve25519.Signing.PublicKey(rawRepresentation: data) {
            publicKey = key
        } else {
            publicKey = nil
            NSLog("[License] public key not configured — license verification disabled")
        }

        loadStoredLicenseFromKeychain()
    }

    private func loadStoredLicenseFromKeychain() {
        let loadGeneration = licenseStateGeneration
        Task.detached(priority: .utility) { [weak self] in
            let stored = LicenseKeychain.load()
            await self?.applyStoredLicense(stored, expectedGeneration: loadGeneration)
        }
    }

    private func applyStoredLicense(_ stored: String?, expectedGeneration: Int) {
        defer {
            initialLoadComplete = true
            NotificationCenter.default.post(name: Self.initialLoadNotification, object: nil)
        }
        guard licenseStateGeneration == expectedGeneration else { return }
        guard let stored, let payload = verify(stored) else { return }
        licenseStateGeneration += 1
        storedLicenseKey = stored
        state = Self.restoredState(
            payload: payload,
            revokedLicenseID: AppDefaults.shared.string(forKey: revokedLicenseIDKey)
        )
        if case .revoked = state {
            // A renewed/recovered subscription should not remain stuck behind
            // the normal seven-day cadence merely because revocation now
            // correctly survives restart.
            Task { await revalidate(stored) }
        } else {
            maybeRevalidate(stored)
        }
    }

    /// A definitive revocation must survive app restart. Otherwise the signed
    /// offline payload would briefly become active again and the fresh
    /// validation timestamp would postpone correction for a week.
    nonisolated static func restoredState(
        payload: LicensePayload,
        revokedLicenseID: String?
    ) -> LicenseState {
        revokedLicenseID == payload.id ? .revoked(payload) : .active(payload)
    }

    // MARK: - Offline verification

    /// Verify a key's signature against the embedded public key and return its
    /// payload, or nil if the key is malformed / forged / for another product.
    /// Normalize a pasted key before parsing. macOS "smart dashes" turns the
    /// hyphen in `CLRTY-…` (and any hyphen inside the base64url body) into a
    /// Unicode dash, which would fail the prefix check and corrupt the
    /// signature bytes — so every Unicode dash is folded back to an ASCII
    /// hyphen. All whitespace is stripped too (line-wrap newlines, stray
    /// spaces, non-breaking/zero-width chars), since the key contains none.
    /// The base64url alphabet is `[A-Za-z0-9-_]` plus `.`, so this only ever
    /// repairs mangling and never changes a well-formed key.
    nonisolated static func normalizeLicenseKey(_ raw: String) -> String {
        let dashes: Set<Character> = [
            "\u{2010}", "\u{2011}", "\u{2012}", "\u{2013}", "\u{2014}",
            "\u{2015}", "\u{2212}", "\u{FE58}", "\u{FE63}", "\u{FF0D}"
        ]
        var out = ""
        out.reserveCapacity(raw.count)
        for character in raw {
            if character.isWhitespace || character == "\u{200B}" { continue }
            out.append(dashes.contains(character) ? "-" : character)
        }
        return out
    }

    func verify(_ rawKey: String) -> LicensePayload? {
        guard let publicKey else { return nil }
        let key = Self.normalizeLicenseKey(rawKey)
        guard key.hasPrefix(LicenseConfig.keyPrefix) else { return nil }
        let body = String(key.dropFirst(LicenseConfig.keyPrefix.count))
        guard let dot = body.firstIndex(of: ".") else { return nil }
        let payloadB64 = String(body[..<dot])
        let sigB64 = String(body[body.index(after: dot)...])
        guard let sig = Self.base64UrlDecode(sigB64),
              let payloadData = Self.base64UrlDecode(payloadB64)
        else { return nil }

        // Signature is over the UTF-8 bytes of the encoded-payload string.
        guard publicKey.isValidSignature(sig, for: Data(payloadB64.utf8)) else { return nil }
        return try? JSONDecoder().decode(LicensePayload.self, from: payloadData)
    }

    // MARK: - Activation (online)

    /// Verify a freshly-entered key offline, then activate this device on the
    /// server. On success the key is stored and the app becomes licensed.
    func activate(_ rawKey: String) async {
        guard !working else { return }
        lastError = nil
        // Normalize first (fixes smart-dash / whitespace mangling from paste)
        // so the same repaired key is what we verify, send, and store.
        let key = Self.normalizeLicenseKey(rawKey)
        guard let payload = verify(key) else {
            lastError = "That doesn't look like a valid Unpeel license key."
            return
        }
        licenseStateGeneration += 1
        let activationGeneration = licenseStateGeneration

        working = true
        defer { working = false }

        let expectedSuppressionGeneration: String?
        do {
            expectedSuppressionGeneration = try RelayUplinkManager.shared
                .activationSuppressionGeneration()
        } catch {
            lastError = "Link's local authority state couldn't be read safely. Try again after fixing its permissions."
            return
        }

        do {
            let result = try await post(
                "/api/activate",
                body: [
                    "key": key,
                    "device_id": Self.deviceID,
                    "device_name": Self.deviceName
                ]
            )
            guard licenseStateGeneration == activationGeneration else {
                if result["ok"] as? Bool == true {
                    _ = try? await post(
                        "/api/deactivate",
                        body: ["key": key, "device_id": Self.deviceID]
                    )
                }
                return
            }
            if result["ok"] as? Bool == true {
                let saved = await Task.detached(priority: .utility) {
                    LicenseKeychain.save(key)
                }.value
                guard saved else {
                    _ = try? await post(
                        "/api/deactivate",
                        body: ["key": key, "device_id": Self.deviceID]
                    )
                    lastError = "The key was accepted, but couldn't be saved securely. Link stayed off; if the seat remains assigned, release it at unpeel.com/account before retrying."
                    return
                }
                guard licenseStateGeneration == activationGeneration else {
                    _ = await Task.detached(priority: .utility) {
                        LicenseKeychain.delete()
                    }.value
                    _ = try? await post(
                        "/api/deactivate",
                        body: ["key": key, "device_id": Self.deviceID]
                    )
                    return
                }
                if let localError = RelayUplinkManager.shared.resumeAfterExplicitActivation(
                    expectedSuppressionGeneration: expectedSuppressionGeneration
                ) {
                    _ = await Task.detached(priority: .utility) {
                        LicenseKeychain.delete()
                    }.value
                    _ = try? await post(
                        "/api/deactivate",
                        body: ["key": key, "device_id": Self.deviceID]
                    )
                    storedLicenseKey = nil
                    state = .unlicensed
                    lastError = "The key was accepted, but Link was disabled while activation finished: \(localError). If the seat remains assigned, release it at unpeel.com/account."
                    return
                }
                storedLicenseKey = key
                persistRevokedLicenseID(nil)
                markValidatedNow()
                state = .active(payload)
                RelayUplinkManager.shared.refresh()
            } else {
                lastError = Self.message(for: result)
                if (result["error"] as? String) == "revoked" { state = .revoked(payload) }
            }
        } catch {
            lastError = "Couldn't reach the activation server. Check your connection and try again."
        }
    }

    /// Release this device's seat and forget the key.
    func deactivate() async {
        let stored = storedLicenseKey
        licenseStateGeneration += 1
        working = true
        defer { working = false }

        // Local authority is revoked before DNS/TCP/API work. A slow or
        // unreachable service must never keep an established Link socket (or
        // its 30-day cached bearer) alive.
        if let error = RelayUplinkManager.shared.deactivateLocalAuthority() {
            lastError = "Link was stopped for this run, but deactivation couldn't be saved: \(error)"
            return
        }
        let keyDeleted = await Task.detached(priority: .utility) {
            LicenseKeychain.delete()
        }.value
        storedLicenseKey = nil
        AppDefaults.shared.removeObject(forKey: lastValidatedKey)
        persistRevokedLicenseID(nil)
        state = .unlicensed
        if !keyDeleted {
            lastError = "Link is off, but the saved key couldn't be removed from Keychain."
        } else {
            lastError = nil
        }

        guard let stored else { return }
        do {
            let result = try await post(
                "/api/deactivate",
                body: ["key": stored, "device_id": Self.deviceID]
            )
            if result["ok"] as? Bool != true {
                lastError = "Link is off locally. Release the seat from your account at unpeel.com/account."
            }
        } catch {
            lastError = "Link is off locally. Release the seat from your account at unpeel.com/account."
        }
    }

    // MARK: - Periodic revalidation

    /// Revalidate if we haven't heard from the server in `recheckInterval`.
    private func maybeRevalidate(_ key: String) {
        let last = AppDefaults.shared.double(forKey: lastValidatedKey)
        let age = Date().timeIntervalSince1970 - last
        guard last == 0 || age > LicenseConfig.recheckInterval else { return }
        Task { await revalidate(key) }
    }

    /// Ask the server whether the license is still valid. Revocation flips the
    /// state; a network failure is tolerated until the offline grace expires.
    func revalidate(_ key: String) async {
        guard storedLicenseKey == key, let payload = verify(key) else { return }
        licenseStateGeneration += 1
        let requestGeneration = licenseStateGeneration
        do {
            let result = try await post(
                "/api/validate",
                body: ["key": key, "device_id": Self.deviceID]
            )
            guard Self.revalidationResponseIsCurrent(
                requestedKey: key,
                currentKey: storedLicenseKey,
                requestGeneration: requestGeneration,
                currentGeneration: licenseStateGeneration
            ) else { return }
            switch Self.validationResponse(for: result) {
            case .revoked:
                // Crash order is authority: persist denial before recording a
                // fresh validation timestamp. Any restart from an intermediate
                // point must restore revoked and refuse a retained cache.
                persistRevokedLicenseID(payload.id)
                state = .revoked(payload)
                let localError = RelayUplinkManager.shared.rejectLocalAuthority()
                markValidatedNow()
                if let localError {
                    lastError = "License revoked; Link stopped, but shutdown couldn't be saved: \(localError)"
                }
            case .active:
                persistRevokedLicenseID(nil)
                state = .active(payload)
                markValidatedNow()
                RelayUplinkManager.shared.refresh()
            case .invalid:
                throw LicenseHTTPError.invalidResponse
            }
        } catch {
            guard Self.revalidationResponseIsCurrent(
                requestedKey: key,
                currentKey: storedLicenseKey,
                requestGeneration: requestGeneration,
                currentGeneration: licenseStateGeneration
            ) else { return }
            // Offline: honor the grace window, then fall back to revoked.
            let last = AppDefaults.shared.double(forKey: lastValidatedKey)
            if last > 0, Date().timeIntervalSince1970 - last > LicenseConfig.offlineGrace {
                let localError = RelayUplinkManager.shared.rejectLocalAuthority()
                persistRevokedLicenseID(payload.id)
                state = .revoked(payload)
                if let localError {
                    lastError = "License validation expired; Link stopped, but shutdown couldn't be saved: \(localError)"
                }
            }
        }
    }

    nonisolated static func revalidationResponseIsCurrent(
        requestedKey: String,
        currentKey: String?,
        requestGeneration: Int,
        currentGeneration: Int
    ) -> Bool {
        requestedKey == currentKey && requestGeneration == currentGeneration
    }

    private func markValidatedNow() {
        AppDefaults.shared.set(Date().timeIntervalSince1970, forKey: lastValidatedKey)
    }

    private func persistRevokedLicenseID(_ licenseID: String?) {
        if let licenseID {
            AppDefaults.shared.set(licenseID, forKey: revokedLicenseIDKey)
        } else {
            AppDefaults.shared.removeObject(forKey: revokedLicenseIDKey)
        }
        // This value is a crash-order authority, not cosmetic preference UI.
        // Flush it before later writes can make a restart skip validation.
        if !AppDefaults.shared.synchronize() {
            NSLog("[License] could not flush persisted revocation state")
        }
    }

    // MARK: - HTTP

    private enum LicenseHTTPError: Error {
        case invalidResponse
        case badStatus(Int)
    }

    private func post(_ path: String, body: [String: String]) async throws -> [String: Any] {
        var request = URLRequest(url: LicenseConfig.apiBaseURL.appendingPathComponent(path))
        request.httpMethod = "POST"
        request.setValue("application/json", forHTTPHeaderField: "Content-Type")
        request.httpBody = try JSONSerialization.data(withJSONObject: body)
        let (data, response) = try await session.data(for: request)
        guard let http = response as? HTTPURLResponse else {
            throw LicenseHTTPError.invalidResponse
        }
        guard (200..<300).contains(http.statusCode) else {
            if let object = try? JSONSerialization.jsonObject(with: data) as? [String: Any] {
                return object
            }
            throw LicenseHTTPError.badStatus(http.statusCode)
        }
        guard let object = try? JSONSerialization.jsonObject(with: data) as? [String: Any] else {
            throw LicenseHTTPError.invalidResponse
        }
        return object
    }

    nonisolated static func validationResponse(for result: [String: Any]) -> ValidationResponse {
        if (result["status"] as? String) == "revoked" {
            return .revoked
        }
        if result["valid"] as? Bool == true {
            return .active
        }
        return .invalid
    }

    nonisolated static func message(for result: [String: Any]) -> String {
        switch result["error"] as? String {
        case "seat_limit":
            if let seats = result["seats"] as? Int {
                return "All \(seats) seats for this license are in use. Deactivate Unpeel "
                    + "on another Mac first, or contact support."
            }
            return "All purchased seats for this license are in use. Deactivate Unpeel on "
                + "another Mac first, or contact support."
        case "revoked":
            return "This license has been revoked."
        case "unknown":
            return "This key isn't recognized. If you just bought Unpeel, try again in a moment."
        default:
            return (result["reason"] as? String) ?? "Activation failed. Please try again."
        }
    }

    // MARK: - Device identity

    /// Stable per-machine id: SHA-256 of the hardware UUID (so the raw UUID
    /// never leaves the device). Same machine → same id across launches.
    static let deviceID: String = {
        let uuid = hardwareUUID() ?? "unknown-mac"
        let digest = SHA256.hash(data: Data(uuid.utf8))
        return digest.map { String(format: "%02x", $0) }.joined()
    }()

    static let deviceName: String = Host.current().localizedName ?? "Mac"

    private static func hardwareUUID() -> String? {
        let service = IOServiceGetMatchingService(
            kIOMainPortDefault, IOServiceMatching("IOPlatformExpertDevice")
        )
        guard service != 0 else { return nil }
        defer { IOObjectRelease(service) }
        guard let cf = IORegistryEntryCreateCFProperty(
            service, kIOPlatformUUIDKey as CFString, kCFAllocatorDefault, 0
        ) else { return nil }
        return cf.takeRetainedValue() as? String
    }

    // MARK: - base64url

    static func base64UrlDecode(_ s: String) -> Data? {
        var b64 = s.replacingOccurrences(of: "-", with: "+").replacingOccurrences(of: "_", with: "/")
        let pad = b64.count % 4
        if pad != 0 { b64 += String(repeating: "=", count: 4 - pad) }
        return Data(base64Encoded: b64)
    }
}
