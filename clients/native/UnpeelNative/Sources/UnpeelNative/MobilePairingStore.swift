import CryptoKit
import Darwin
import Foundation
import ImageIO
import Security
import UniformTypeIdentifiers
import UnpeelShared


struct MobileRemoteError: Error {
    let status: Int
    let message: String

    init(_ status: Int, _ message: String) {
        self.status = status
        self.message = message
    }
}

private struct MobilePairedDeviceFile: Codable {
    var version: Int = 1
    var devices: [MobilePairedDeviceRecord] = []
}

struct MobilePairedDeviceRecord: Codable, Equatable {
    var id: String
    var name: String
    var platform: String
    var appVersion: String?
    /// Stable human identity, separate from this revocable device. Existing
    /// records omit it and resolve to the Host owner at authentication time.
    var principalID: String?
    var tokenHash: String
    var pairedAtUnixMs: Int64
    var lastSeenAtUnixMs: Int64?
    /// The E2E key lives in the login Keychain and the locked, 0600 shared
    /// Host store used by the standalone TUI. The relay token is stored
    /// hash-only: the raw value lives solely on the phone.
    var relayTokenHash: String
    /// APNs device token (hex) + environment (`sandbox`/`production`) for push
    /// notifications, registered post-pairing via `/mobile/push-token`. Nil
    /// until the phone reports one (permission granted + remote registration).
    var apnsToken: String?
    var apnsEnvironment: String?
    /// Per-device Unpeel Link scope: false keeps this device Direct/LAN-only —
    /// its relay token is never registered with the uplink. Nil means allowed,
    /// so pre-flag records and every fresh pairing keep today's behavior.
    var relayAllowed: Bool?
    /// One pairing = one workspace (2026-08-23): true on every pairing minted
    /// since. A scoped device cannot `select-workspace` to a sibling, and its
    /// bootstrap advertises only the current workspace. Nil = a legacy pairing
    /// with the shipped Host-level trust, kept working for compatibility.
    var workspaceScoped: Bool?

    var isRelayAllowed: Bool { relayAllowed ?? true }

    var isWorkspaceScoped: Bool { workspaceScoped ?? false }

    var summary: RemotePairedDeviceSummary {
        RemotePairedDeviceSummary(
            id: id,
            name: name,
            platform: platform,
            appVersion: appVersion,
            pairedAtUnixMs: pairedAtUnixMs,
            lastSeenAtUnixMs: lastSeenAtUnixMs,
            relayAllowed: relayAllowed
        )
    }
}

private struct ActiveMobilePairing {
    var token: String
    var endpoint: URL
    var directEndpoint: URL
    var expiresAtUnixMs: Int64
}

protocol MobileE2EKeyStoring: AnyObject {
    func load(deviceID: String) -> Data?
    func save(_ key: Data, deviceID: String) throws
    func delete(deviceID: String)
}

final class MobileE2EKeychainStore: MobileE2EKeyStoring {
    private let service = "com.unpeel.mobile.e2e"
    /// Keychain items are bundle-scoped, not UNPEEL_HOME-scoped, so the
    /// account must carry this instance's macID: the phone reuses ONE
    /// deviceID across every Mac/workspace it pairs with, and a bare-deviceID
    /// account would let a second workspace's pairing overwrite the first
    /// workspace's per-device static relay key.
    private let macID: String

    init(macID: String) {
        self.macID = macID
    }

    func load(deviceID: String) -> Data? {
        if let data = loadData(account: scopedAccount(deviceID: deviceID)) {
            return data
        }
        // Pairings created before workspace support used the bare deviceID:
        // copy forward under the scoped account (keep the legacy item —
        // rollback safety).
        // No cross-workspace bleed: this only runs for devices already in this
        // instance's own devices.json, and new pairings write scoped.
        guard let legacy = loadData(account: deviceID) else { return nil }
        try? save(legacy, deviceID: deviceID)
        return legacy
    }

    func save(_ key: Data, deviceID: String) throws {
        let query = baseQuery(account: scopedAccount(deviceID: deviceID))
        let update = SecItemUpdate(
            query as CFDictionary,
            [kSecValueData as String: key] as CFDictionary
        )
        if update == errSecSuccess { return }
        guard update == errSecItemNotFound else { throw MobileRemoteError(500, "keychain update failed") }
        var add = query
        add[kSecValueData as String] = key
        add[kSecAttrAccessible as String] = kSecAttrAccessibleAfterFirstUnlockThisDeviceOnly
        guard SecItemAdd(add as CFDictionary, nil) == errSecSuccess else {
            throw MobileRemoteError(500, "keychain save failed")
        }
    }

    func delete(deviceID: String) {
        SecItemDelete(baseQuery(account: scopedAccount(deviceID: deviceID)) as CFDictionary)
        // Also drop the legacy bare-deviceID item; deleting a missing item
        // is a no-op, and revocation should leave no copy behind.
        SecItemDelete(baseQuery(account: deviceID) as CFDictionary)
    }

    private func scopedAccount(deviceID: String) -> String {
        "\(macID).\(deviceID)"
    }

    private func loadData(account: String) -> Data? {
        var query = baseQuery(account: account)
        query[kSecReturnData as String] = true
        query[kSecMatchLimit as String] = kSecMatchLimitOne
        var result: AnyObject?
        guard SecItemCopyMatching(query as CFDictionary, &result) == errSecSuccess else { return nil }
        return result as? Data
    }

    private func baseQuery(account: String) -> [String: Any] {
        [
            kSecClass as String: kSecClassGenericPassword,
            kSecAttrService as String: service,
            kSecAttrAccount as String: account,
        ]
    }
}

final class MobilePairingStore: @unchecked Sendable {
    let macID: String
    let macName: String

    private let storageURL: URL
    private let e2eKeyStore: MobileE2EKeyStoring
    /// `flock` coordinates native and TUI processes. This process lock also
    /// serializes multiple MobilePairingStore instances because BSD `flock`
    /// semantics alone are not a portable same-process mutex.
    private static let processStorageLock = NSLock()
    private let lock = NSLock()
    private var activePairing: ActiveMobilePairing?
    private var records: [MobilePairedDeviceRecord] = []

    init(
        storageURL: URL = LaunchConfig.unpeelDir
            .appendingPathComponent("mobile")
            .appendingPathComponent("devices.json"),
        macID: String = MobilePairingStore.defaultMacID(),
        macName: String = UnpeelWorkspaceContext.advertisedHostName,
        e2eKeyStore: MobileE2EKeyStoring? = nil
    ) {
        self.storageURL = storageURL
        self.macID = macID
        self.macName = macName
        // Default resolved here, not in the parameter list, because the
        // keychain account is scoped by this store's macID.
        self.e2eKeyStore = e2eKeyStore ?? MobileE2EKeychainStore(macID: macID)
        // Reconcile while native still has Keychain access. This makes an
        // existing app pairing usable by the standalone TUI without asking
        // the phone to pair again. The locked authority file is reloaded
        // before reconciliation; a valid shared revision wins so a key that
        // the TUI rotated is copied back into the scoped Keychain account.
        do {
            try withFreshRecords { directoryDescriptor in
                try reconcileSharedE2EKeysLocked(
                    directoryDescriptor: directoryDescriptor
                )
            }
        } catch {
            // Construction cannot throw. Link fails closed, but devices.json
            // remains the independent authority for Direct/LAN access and an
            // explicit revoke must still be able to remove that authority.
            NSLog("[UnpeelNative] failed to reconcile mobile E2E keys: \(error)")
        }
    }

    var devices: [RemotePairedDeviceSummary] {
        (try? withFreshRecords { _ in records.map(\.summary) }) ?? []
    }

    /// Mirror worker-created shared E2E revisions into the scoped Keychain.
    /// `devices.json` remains the authorization authority; only authorized
    /// records participate, under the same cross-process transaction lock.
    func reconcilePlatformE2EKeys() throws {
        try withFreshRecords { directoryDescriptor in
            try reconcileSharedE2EKeysLocked(directoryDescriptor: directoryDescriptor)
        }
    }

    /// Remove the Keychain mirror only after the Rust Host has durably
    /// removed this device from `devices.json`. A stale/out-of-order callback
    /// fails closed instead of deleting a key for a newly re-paired device.
    func removePlatformE2EKey(deviceID: String) throws {
        try withFreshRecords { _ in
            guard !records.contains(where: { $0.id == deviceID }) else {
                throw MobileRemoteError(409, "device is still authorized")
            }
            e2eKeyStore.delete(deviceID: deviceID)
        }
    }

    func principalID(forDeviceID deviceID: String) -> String? {
        try? withFreshRecords { _ in
            guard let record = records.first(where: { $0.id == deviceID }) else { return nil }
            let stored = record.principalID?
                .trimmingCharacters(in: .whitespacesAndNewlines)
            return stored?.isEmpty == false
                ? stored
                : SessionOwnership.hostOwnerPrincipalID(hostID: macID)
        }
    }

    func beginPairing(
        endpoint: URL,
        directEndpoint: URL? = nil,
        now: Date = Date(),
        ttlSeconds: TimeInterval = 5 * 60
    ) -> RemotePairingPayload {
        // Uppercase base32 keeps the compact pairing code (RemotePairingCode)
        // inside the QR alphanumeric charset — a visibly coarser code than
        // base64url forced. 16 one-time bytes with a 5-minute TTL is ample;
        // the durable per-device tokens stay 32-byte base64url.
        let token = Self.randomBase32Token(byteCount: 16)
        let expiresAt = Self.unixMs(now.addingTimeInterval(ttlSeconds))
        lock.withLock {
            activePairing = ActiveMobilePairing(
                token: token,
                endpoint: endpoint,
                directEndpoint: directEndpoint ?? endpoint,
                expiresAtUnixMs: expiresAt
            )
        }
        return RemotePairingPayload(
            macID: macID,
            macName: macName,
            endpoint: endpoint,
            token: token,
            certificateFingerprint: nil,
            expiresAtUnixMs: expiresAt
        )
    }

    func cancelPairing() {
        lock.withLock {
            activePairing = nil
        }
    }

    func pair(_ request: RemotePairingRequest, now: Date = Date()) throws -> RemotePairingResponse {
        let response = try pairLocked(request, now: now)
        // Lets in-process observers refresh their device lists.
        NotificationCenter.default.post(name: .unpeelMobileDevicesChanged, object: nil)
        return response
    }

    /// Open the LAN pairing request with the currently displayed QR secret.
    /// Pairing deliberately stays on the tiny HTTP bootstrap server, but no
    /// credential-bearing byte is plaintext or unauthenticated on the LAN.
    func decryptPairingRequest(_ envelope: RemotePairingEnvelope) throws -> RemotePairingRequest {
        let context = try lock.withLock { () throws -> ActiveMobilePairing in
            guard let activePairing else { throw MobileRemoteError(401, "pairing is not active") }
            guard activePairing.expiresAtUnixMs > Self.unixMs(Date()) else {
                self.activePairing = nil
                throw MobileRemoteError(401, "pairing token expired")
            }
            return activePairing
        }
        do {
            let plaintext = try RemotePairingCrypto.open(
                envelope,
                token: context.token,
                macID: macID,
                endpoint: context.endpoint,
                direction: .request
            )
            return try JSONDecoder().decode(RemotePairingRequest.self, from: plaintext)
        } catch let error as MobileRemoteError {
            throw error
        } catch {
            throw MobileRemoteError(401, "invalid encrypted pairing request")
        }
    }

    private func pairLocked(
        _ request: RemotePairingRequest,
        now: Date
    ) throws -> RemotePairingResponse {
        try withFreshRecords { directoryDescriptor in
            try reconcileSharedE2EKeysLocked(
                directoryDescriptor: directoryDescriptor
            )
            guard let activePairing else {
                throw MobileRemoteError(401, "pairing is not active")
            }
            let nowMs = Self.unixMs(now)
            guard activePairing.expiresAtUnixMs > nowMs else {
                self.activePairing = nil
                throw MobileRemoteError(401, "pairing token expired")
            }
            guard request.token == activePairing.token else {
                throw MobileRemoteError(401, "invalid pairing token")
            }

            let authToken = Self.randomToken(byteCount: 32)
            let deviceID = request.device.id
                .trimmingCharacters(in: .whitespacesAndNewlines)
                .isEmpty ? UUID().uuidString.lowercased() : request.device.id
            // Unpeel Remote credentials ride the same pairing exchange: the
            // E2E key and relay token reach the phone over the LAN channel
            // the scanned one-time token just authenticated.
            let previousKey = e2eKeyStore.load(deviceID: deviceID)
            let previousSharedValue = try sharedE2EKeyValue(
                deviceID: deviceID,
                directoryDescriptor: directoryDescriptor
            )
            let e2eKey = Self.randomBytes(32)
            let relayToken = Self.randomToken(byteCount: 32)
            try e2eKeyStore.save(e2eKey, deviceID: deviceID)
            do {
                try saveSharedE2EKey(
                    e2eKey,
                    deviceID: deviceID,
                    directoryDescriptor: directoryDescriptor
                )
            } catch {
                restoreKeychain(previousKey, deviceID: deviceID)
                throw error
            }
            let record = MobilePairedDeviceRecord(
                id: deviceID,
                name: request.device.name,
                platform: request.device.platform,
                appVersion: request.device.appVersion,
                principalID: SessionOwnership.hostOwnerPrincipalID(hostID: macID),
                tokenHash: Self.sha256(authToken),
                pairedAtUnixMs: nowMs,
                lastSeenAtUnixMs: nowMs,
                relayTokenHash: Self.sha256(relayToken),
                workspaceScoped: true
            )

            records.removeAll { $0.id == record.id }
            records.append(record)
            do {
                try persistLocked(directoryDescriptor: directoryDescriptor)
            } catch {
                // Keep the Keychain and devices.json one logical credential
                // revision. In particular, a failed re-pair must not replace
                // the old device's E2E key while its old authority record is
                // still the durable one.
                restoreKeychain(previousKey, deviceID: deviceID)
                restoreSharedE2EKeyValue(
                    previousSharedValue,
                    deviceID: deviceID,
                    directoryDescriptor: directoryDescriptor
                )
                throw error
            }
            self.activePairing = nil

            return RemotePairingResponse(
                macID: macID,
                macName: macName,
                endpoint: activePairing.endpoint,
                directEndpoint: activePairing.directEndpoint == activePairing.endpoint
                    ? nil : activePairing.directEndpoint,
                deviceID: record.id,
                authToken: authToken,
                pairedAtUnixMs: nowMs,
                relayCredentials: RelayCredentials(
                    relayURL: RelayConfig.relayURL,
                    macID: macID,
                    relayToken: relayToken,
                    e2eKey: e2eKey
                )
            )
        }
    }

    /// Hashes the relay Worker validates client connects against; the host
    /// uplink registers them in its hello frame. Devices with relay disallowed
    /// are simply never registered — the relay refuses their token and they
    /// fail closed to Direct.
    func relayTokenRegistrations() -> [RelayDeviceTokenRegistration] {
        (try? withFreshRecords { _ in
            records.filter(\.isRelayAllowed).map { record in
                RelayDeviceTokenRegistration(deviceID: record.id, tokenHash: record.relayTokenHash)
            }
        }) ?? []
    }

    func relayAllowed(forDeviceID deviceID: String) -> Bool {
        relayTokenHash(forDeviceID: deviceID) != nil
    }

    /// The exact Link authorization revision for one device. Binding an
    /// established crypto session to this value (not merely the stable device
    /// id) makes credential rotation/re-pair revoke the old session even when
    /// another frontend changed devices.json and an in-process notification
    /// was missed.
    func relayTokenHash(forDeviceID deviceID: String) -> String? {
        try? withFreshRecords { _ in
            guard let record = records.first(where: { $0.id == deviceID }),
                  record.isRelayAllowed else { return nil }
            return record.relayTokenHash
        }
    }

    /// Scope a paired device to Direct-only (or back). Posting the devices
    /// change replaces the uplink, so the relay's registered token set and any
    /// in-flight relay connection for the device drop immediately.
    func setRelayAllowed(deviceID: String, allowed: Bool) {
        let changed: Bool
        do {
            changed = try withFreshRecords { directoryDescriptor in
                guard let index = records.firstIndex(where: { $0.id == deviceID }),
                      records[index].isRelayAllowed != allowed else { return false }
                // Allowed is the nil default: store only the narrowing value
                // so the file gains no key until a device is restricted.
                records[index].relayAllowed = allowed ? nil : false
                try persistLocked(directoryDescriptor: directoryDescriptor)
                return true
            }
        } catch {
            NSLog("[UnpeelNative] failed to change device relay scope: \(error)")
            changed = false
        }
        if changed {
            NotificationCenter.default.post(name: .unpeelMobileDevicesChanged, object: nil)
        }
    }

    /// Whether this device's pairing is scoped to one workspace (every
    /// pairing minted since 2026-08-23). Unknown devices answer scoped —
    /// the narrow default; authentication has already failed them anyway.
    func isWorkspaceScoped(deviceID: String) -> Bool {
        (try? withFreshRecords { _ in
            records.first { $0.id == deviceID }?.isWorkspaceScoped ?? true
        }) ?? true
    }

    /// Rewrite a device's workspace-scope stamp. The only sanctioned use is
    /// fabricating LEGACY records (scoped → nil) in tests that cover shipped
    /// pre-2026-08-23 devices; nothing in the product widens a pairing.
    func setWorkspaceScopedForCompatTesting(_ scoped: Bool?, deviceID: String) {
        do {
            try withFreshRecords { directoryDescriptor in
                guard let index = records.firstIndex(where: { $0.id == deviceID }) else { return }
                records[index].workspaceScoped = scoped
                try persistLocked(directoryDescriptor: directoryDescriptor)
            }
        } catch {
            NSLog("[UnpeelNative] failed to change device workspace scope: \(error)")
        }
    }

    func e2eKey(forDeviceID deviceID: String) -> Data? {
        try? withFreshRecords { directoryDescriptor in
            guard records.contains(where: { $0.id == deviceID }) else { return nil }
            try reconcileSharedE2EKeysLocked(
                directoryDescriptor: directoryDescriptor
            )
            return try loadSharedE2EKey(
                deviceID: deviceID,
                directoryDescriptor: directoryDescriptor
            )
        }
    }

    /// Mint fresh relay credentials after credential loss. Reported write
    /// failures restore the previous key/token revision. The flat shared-store
    /// compatibility ABI cannot make a process/power-loss window spanning
    /// Keychain, shared keys, and devices.json crash-atomic.
    func rotateRelayCredentials(deviceID: String) -> RelayCredentials? {
        let credentials: RelayCredentials?
        do {
            credentials = try withFreshRecords { directoryDescriptor in
                try reconcileSharedE2EKeysLocked(
                    directoryDescriptor: directoryDescriptor
                )
                guard let index = records.firstIndex(where: { $0.id == deviceID }) else {
                    return nil
                }
                let previousKey = e2eKeyStore.load(deviceID: deviceID)
                let previousSharedValue = try sharedE2EKeyValue(
                    deviceID: deviceID,
                    directoryDescriptor: directoryDescriptor
                )
                let e2eKey = Self.randomBytes(32)
                let relayToken = Self.randomToken(byteCount: 32)
                try e2eKeyStore.save(e2eKey, deviceID: deviceID)
                do {
                    try saveSharedE2EKey(
                        e2eKey,
                        deviceID: deviceID,
                        directoryDescriptor: directoryDescriptor
                    )
                } catch {
                    restoreKeychain(previousKey, deviceID: deviceID)
                    throw error
                }
                records[index].relayTokenHash = Self.sha256(relayToken)
                do {
                    try persistLocked(directoryDescriptor: directoryDescriptor)
                } catch {
                    restoreKeychain(previousKey, deviceID: deviceID)
                    restoreSharedE2EKeyValue(
                        previousSharedValue,
                        deviceID: deviceID,
                        directoryDescriptor: directoryDescriptor
                    )
                    throw error
                }
                return RelayCredentials(
                    relayURL: RelayConfig.relayURL,
                    macID: macID,
                    relayToken: relayToken,
                    e2eKey: e2eKey
                )
            }
        } catch {
            NSLog("[UnpeelNative] failed to rotate relay credentials: \(error)")
            credentials = nil
        }
        if credentials != nil {
            // The uplink re-sends its hello (fresh hash set) on this signal.
            NotificationCenter.default.post(name: .unpeelMobileDevicesChanged, object: nil)
        }
        return credentials
    }

    /// Register (or refresh) a device's APNs token so the Mac can push to it.
    /// Called from the authenticated `/mobile/push-token` route.
    @discardableResult
    func setPushToken(
        deviceID: String,
        token: String,
        environment: String
    ) throws -> Bool? {
        try withFreshRecords { directoryDescriptor in
            guard let index = records.firstIndex(where: { $0.id == deviceID }) else { return nil }
            guard records[index].apnsToken != token
                || records[index].apnsEnvironment != environment else { return false }
            records[index].apnsToken = token
            records[index].apnsEnvironment = environment
            try persistLocked(directoryDescriptor: directoryDescriptor)
            return true
        }
    }

    /// Drop a device's APNs token after APNs reports it dead
    /// (BadDeviceToken/Unregistered), so the Mac stops pushing to it.
    func clearPushToken(deviceID: String) {
        do {
            try withFreshRecords { directoryDescriptor in
                guard let index = records.firstIndex(where: { $0.id == deviceID }),
                      records[index].apnsToken != nil else { return }
                records[index].apnsToken = nil
                records[index].apnsEnvironment = nil
                try persistLocked(directoryDescriptor: directoryDescriptor)
            }
        } catch {
            NSLog("[UnpeelNative] failed to clear push token: \(error)")
        }
    }

    /// Every paired device with a registered push token, for fan-out.
    func pushTargets() -> [(deviceID: String, token: String, environment: String)] {
        (try? withFreshRecords { _ in
            records.compactMap { record in
                // Pushes travel through the Unpeel Link service, so a device
                // scoped Direct-only is not a push target either — "Direct
                // only" means nothing about this device rides the relay.
                guard record.isRelayAllowed else { return nil }
                guard let token = record.apnsToken, !token.isEmpty else { return nil }
                return (record.id, token, record.apnsEnvironment ?? "production")
            }
        }) ?? []
    }

    func verifyAuthorizationHeader(_ header: String?, now: Date = Date()) -> String? {
        guard let token = Self.bearerToken(from: header) else { return nil }
        let tokenHash = Self.sha256(token)
        do {
            return try withFreshRecords { directoryDescriptor in
                guard let index = records.firstIndex(where: { $0.tokenHash == tokenHash }) else {
                    return nil
                }
                let id = records[index].id
                let nowMs = Self.unixMs(now)
                let previous = records[index].lastSeenAtUnixMs ?? 0
                if nowMs - previous > 60_000 {
                    records[index].lastSeenAtUnixMs = nowMs
                    do {
                        try persistLocked(directoryDescriptor: directoryDescriptor)
                    } catch {
                        // lastSeen is diagnostic only. The credential was read
                        // from the locked authority file and remains valid;
                        // do not turn a metadata write failure into auth churn.
                        records = try Self.loadRecordsStrict(
                            directoryDescriptor: directoryDescriptor,
                            fileName: storageURL.lastPathComponent
                        )
                        NSLog("[UnpeelNative] failed to persist device lastSeen: \(error)")
                    }
                }
                return id
            }
        } catch {
            // A missing, locked, or malformed authority file fails closed.
            return nil
        }
    }

    @discardableResult
    func revokeDevice(id: String) -> Bool {
        let changed: Bool
        do {
            changed = try withFreshRecords { directoryDescriptor in
                guard records.contains(where: { $0.id == id }) else { return false }
                records.removeAll { $0.id == id }
                // Commit the authorization removal first. Key cleanup is
                // intentionally after the durable authority change, while the
                // same cross-process lock still excludes a re-pair of this id.
                try persistLocked(directoryDescriptor: directoryDescriptor)
                do {
                    try removeSharedE2EKey(
                        deviceID: id,
                        directoryDescriptor: directoryDescriptor
                    )
                } catch {
                    // Authorization is already durably gone. A leftover key
                    // is an unusable orphan, so cleanup failure must not make
                    // the caller believe revocation failed.
                    NSLog("[UnpeelNative] failed to clean revoked shared E2E key: \(error)")
                }
                e2eKeyStore.delete(deviceID: id)
                return true
            }
        } catch {
            NSLog("[UnpeelNative] failed to revoke mobile device: \(error)")
            changed = false
        }
        if changed {
            // Lets the server/uplink close active access or stop entirely.
            NotificationCenter.default.post(name: .unpeelMobileDevicesChanged, object: nil)
        }
        return changed
    }

    /// Serialize every native/TUI read-modify-write through the stable
    /// `devices.lock` inode, and always reload the authority after acquiring
    /// it. Atomic rename alone protects readers from torn JSON but does not
    /// prevent two frontends from overwriting each other's newer snapshot.
    private func withFreshRecords<T>(_ operation: (Int32) throws -> T) throws -> T {
        try lock.withLock {
            Self.processStorageLock.lock()
            defer { Self.processStorageLock.unlock() }

            let directory = storageURL.deletingLastPathComponent()
            try FileManager.default.createDirectory(
                at: directory,
                withIntermediateDirectories: true
            )
            let directoryDescriptor = open(
                directory.path,
                O_RDONLY | O_DIRECTORY | O_NOFOLLOW | O_CLOEXEC | O_NONBLOCK
            )
            guard directoryDescriptor >= 0 else {
                throw MobileRemoteError(500, "device store directory open failed")
            }
            defer { _ = close(directoryDescriptor) }
            try Self.requireFileKind(
                descriptor: directoryDescriptor,
                kind: mode_t(S_IFDIR),
                message: "device store directory is unsafe"
            )

            let descriptor = "devices.lock".withCString { name in
                openat(
                    directoryDescriptor,
                    name,
                    O_CREAT | O_RDWR | O_CLOEXEC | O_NOFOLLOW | O_NONBLOCK,
                    mode_t(0o600)
                )
            }
            guard descriptor >= 0 else {
                throw MobileRemoteError(500, "device store lock open failed")
            }
            defer { _ = close(descriptor) }
            try Self.requirePrivateRegularFile(
                descriptor: descriptor,
                message: "device store lock is unsafe"
            )
            guard fchmod(descriptor, mode_t(0o600)) == 0 else {
                throw MobileRemoteError(500, "device store lock permission failed")
            }
            guard flock(descriptor, LOCK_EX) == 0 else {
                throw MobileRemoteError(500, "device store lock failed")
            }
            defer { _ = flock(descriptor, LOCK_UN) }

            records = try Self.loadRecordsStrict(
                directoryDescriptor: directoryDescriptor,
                fileName: storageURL.lastPathComponent
            )
            do {
                return try operation(directoryDescriptor)
            } catch {
                // A failed mutation must not remain authoritative in memory.
                records = (try? Self.loadRecordsStrict(
                    directoryDescriptor: directoryDescriptor,
                    fileName: storageURL.lastPathComponent
                )) ?? []
                throw error
            }
        }
    }

    private func persistLocked(directoryDescriptor: Int32) throws {
        let data = try JSONEncoder().encode(MobilePairedDeviceFile(devices: records))
        try Self.writePrivateAtomically(
            data,
            directoryDescriptor: directoryDescriptor,
            fileName: storageURL.lastPathComponent,
            description: "device store"
        )
    }

    /// Reconcile the Keychain and the CLI-readable flat map while holding the
    /// same `devices.lock` used for the authority file. Only authorized device
    /// records participate. A valid shared revision wins (TUI rotation), and
    /// a missing shared revision is copied from scoped-or-legacy Keychain.
    private func reconcileSharedE2EKeysLocked(directoryDescriptor: Int32) throws {
        var values = try Self.loadSharedE2EKeyValues(
            directoryDescriptor: directoryDescriptor
        )
        var changed = false
        for record in records {
            let name = sharedE2EKeyName(deviceID: record.id)
            if let encoded = values[name] {
                let shared = try Self.decodeSharedE2EKey(encoded)
                if e2eKeyStore.load(deviceID: record.id) != shared {
                    try e2eKeyStore.save(shared, deviceID: record.id)
                }
            } else if let keychain = e2eKeyStore.load(deviceID: record.id) {
                guard keychain.count == 32 else {
                    throw MobileRemoteError(500, "Keychain E2E key is invalid")
                }
                values[name] = keychain.base64EncodedString()
                changed = true
            }
        }
        if changed {
            try Self.saveSharedE2EKeyValues(
                values,
                directoryDescriptor: directoryDescriptor
            )
        }
    }

    private func sharedE2EKeyName(deviceID: String) -> String {
        "\(macID).\(deviceID)"
    }

    private func sharedE2EKeyValue(
        deviceID: String,
        directoryDescriptor: Int32
    ) throws -> String? {
        let value = try Self.loadSharedE2EKeyValues(
            directoryDescriptor: directoryDescriptor
        )[sharedE2EKeyName(deviceID: deviceID)]
        if let value {
            _ = try Self.decodeSharedE2EKey(value)
        }
        return value
    }

    private func loadSharedE2EKey(
        deviceID: String,
        directoryDescriptor: Int32
    ) throws -> Data? {
        guard let value = try sharedE2EKeyValue(
            deviceID: deviceID,
            directoryDescriptor: directoryDescriptor
        ) else { return nil }
        return try Self.decodeSharedE2EKey(value)
    }

    private func saveSharedE2EKey(
        _ key: Data,
        deviceID: String,
        directoryDescriptor: Int32
    ) throws {
        guard key.count == 32 else {
            throw MobileRemoteError(500, "E2E key is invalid")
        }
        var values = try Self.loadSharedE2EKeyValues(
            directoryDescriptor: directoryDescriptor
        )
        values[sharedE2EKeyName(deviceID: deviceID)] = key.base64EncodedString()
        try Self.saveSharedE2EKeyValues(values, directoryDescriptor: directoryDescriptor)
    }

    private func removeSharedE2EKey(
        deviceID: String,
        directoryDescriptor: Int32
    ) throws {
        var values = try Self.loadSharedE2EKeyValues(
            directoryDescriptor: directoryDescriptor
        )
        guard values.removeValue(forKey: sharedE2EKeyName(deviceID: deviceID)) != nil else {
            return
        }
        try Self.saveSharedE2EKeyValues(values, directoryDescriptor: directoryDescriptor)
    }

    private func restoreKeychain(_ previousKey: Data?, deviceID: String) {
        if let previousKey {
            do {
                try e2eKeyStore.save(previousKey, deviceID: deviceID)
            } catch {
                // The restored shared revision remains canonical and will be
                // copied back on the next native locked read.
                NSLog("[UnpeelNative] failed to roll back Keychain E2E key: \(error)")
            }
        } else {
            e2eKeyStore.delete(deviceID: deviceID)
        }
    }

    private func restoreSharedE2EKeyValue(
        _ previousValue: String?,
        deviceID: String,
        directoryDescriptor: Int32
    ) {
        do {
            var values = try Self.loadSharedE2EKeyValues(
                directoryDescriptor: directoryDescriptor
            )
            let name = sharedE2EKeyName(deviceID: deviceID)
            if let previousValue {
                values[name] = previousValue
            } else {
                values.removeValue(forKey: name)
            }
            try Self.saveSharedE2EKeyValues(
                values,
                directoryDescriptor: directoryDescriptor
            )
        } catch {
            NSLog("[UnpeelNative] failed to roll back shared E2E key: \(error)")
        }
    }

    private static func loadSharedE2EKeyValues(
        directoryDescriptor: Int32
    ) throws -> [String: String] {
        guard let data = try readPrivateRegularFile(
            directoryDescriptor: directoryDescriptor,
            fileName: "e2e-keys.json",
            description: "shared E2E key store"
        ) else { return [:] }
        do {
            return try JSONDecoder().decode([String: String].self, from: data)
        } catch {
            throw MobileRemoteError(500, "shared E2E key store is malformed")
        }
    }

    private static func saveSharedE2EKeyValues(
        _ values: [String: String],
        directoryDescriptor: Int32
    ) throws {
        do {
            var data = try JSONSerialization.data(
                withJSONObject: values,
                options: [.prettyPrinted, .sortedKeys]
            )
            data.append(0x0A)
            try writePrivateAtomically(
                data,
                directoryDescriptor: directoryDescriptor,
                fileName: "e2e-keys.json",
                description: "shared E2E key store"
            )
        } catch {
            if error is MobileRemoteError { throw error }
            throw MobileRemoteError(500, "shared E2E key store encoding failed")
        }
    }

    private static func decodeSharedE2EKey(_ encoded: String) throws -> Data {
        guard let key = Data(base64Encoded: encoded),
              key.count == 32,
              key.base64EncodedString() == encoded
        else {
            throw MobileRemoteError(500, "shared E2E key is invalid")
        }
        return key
    }

    private static func loadRecordsStrict(
        directoryDescriptor: Int32,
        fileName: String
    ) throws -> [MobilePairedDeviceRecord] {
        guard let data = try readPrivateRegularFile(
            directoryDescriptor: directoryDescriptor,
            fileName: fileName,
            description: "device store"
        ) else { return [] }
        do {
            return try JSONDecoder().decode(MobilePairedDeviceFile.self, from: data).devices
        } catch {
            throw MobileRemoteError(500, "device store is unreadable")
        }
    }

    private static func readPrivateRegularFile(
        directoryDescriptor: Int32,
        fileName: String,
        description: String
    ) throws -> Data? {
        try requireSafeFileName(fileName)
        let descriptor = fileName.withCString { name in
            openat(
                directoryDescriptor,
                name,
                O_RDONLY | O_CLOEXEC | O_NOFOLLOW | O_NONBLOCK
            )
        }
        guard descriptor >= 0 else {
            if errno == ENOENT { return nil }
            throw MobileRemoteError(500, "\(description) open failed")
        }
        defer { _ = close(descriptor) }
        try requirePrivateRegularFile(
            descriptor: descriptor,
            message: "\(description) is unsafe"
        )
        guard fchmod(descriptor, mode_t(0o600)) == 0 else {
            throw MobileRemoteError(500, "\(description) permission failed")
        }

        var metadata = stat()
        guard fstat(descriptor, &metadata) == 0,
              metadata.st_size >= 0,
              metadata.st_size <= 4 * 1024 * 1024
        else {
            throw MobileRemoteError(500, "\(description) is too large")
        }

        var data = Data()
        data.reserveCapacity(Int(metadata.st_size))
        var buffer = [UInt8](repeating: 0, count: 16 * 1024)
        while true {
            let count = buffer.withUnsafeMutableBytes { bytes in
                Darwin.read(descriptor, bytes.baseAddress, bytes.count)
            }
            if count < 0 {
                if errno == EINTR { continue }
                throw MobileRemoteError(500, "\(description) read failed")
            }
            if count == 0 { break }
            guard data.count + count <= 4 * 1024 * 1024 else {
                throw MobileRemoteError(500, "\(description) is too large")
            }
            data.append(buffer, count: count)
        }
        return data
    }

    private static func writePrivateAtomically(
        _ data: Data,
        directoryDescriptor: Int32,
        fileName: String,
        description: String
    ) throws {
        try requireSafeFileName(fileName)
        try requireRegularFileOrMissing(
            directoryDescriptor: directoryDescriptor,
            fileName: fileName,
            description: description
        )

        let temporaryName = ".\(fileName).\(getpid()).\(UUID().uuidString).tmp"
        let descriptor = temporaryName.withCString { name in
            openat(
                directoryDescriptor,
                name,
                O_CREAT | O_EXCL | O_WRONLY | O_CLOEXEC | O_NOFOLLOW | O_NONBLOCK,
                mode_t(0o600)
            )
        }
        guard descriptor >= 0 else {
            throw MobileRemoteError(500, "\(description) temporary file open failed")
        }

        var needsClose = true
        var committed = false
        defer {
            if needsClose { _ = close(descriptor) }
            if !committed {
                temporaryName.withCString { name in
                    _ = unlinkat(directoryDescriptor, name, 0)
                }
            }
        }

        try requirePrivateRegularFile(
            descriptor: descriptor,
            message: "\(description) temporary file is unsafe"
        )
        guard fchmod(descriptor, mode_t(0o600)) == 0 else {
            throw MobileRemoteError(500, "\(description) permission failed")
        }
        try data.withUnsafeBytes { bytes in
            guard let base = bytes.baseAddress else { return }
            var offset = 0
            while offset < bytes.count {
                let written = Darwin.write(
                    descriptor,
                    base.advanced(by: offset),
                    bytes.count - offset
                )
                if written < 0 {
                    if errno == EINTR { continue }
                    throw MobileRemoteError(500, "\(description) write failed")
                }
                guard written > 0 else {
                    throw MobileRemoteError(500, "\(description) short write")
                }
                offset += written
            }
        }
        guard fsync(descriptor) == 0 else {
            throw MobileRemoteError(500, "\(description) sync failed")
        }
        let closeResult = close(descriptor)
        needsClose = false
        guard closeResult == 0 else {
            throw MobileRemoteError(500, "\(description) close failed")
        }

        let renameResult = temporaryName.withCString { temporary in
            fileName.withCString { final in
                renameat(directoryDescriptor, temporary, directoryDescriptor, final)
            }
        }
        guard renameResult == 0 else {
            throw MobileRemoteError(500, "\(description) commit failed")
        }
        committed = true
        // The file was synced before rename. Directory fsync makes that
        // rename durable; after the commit point, never claim failure and
        // trigger a rollback that could disagree with the live revision.
        _ = fsync(directoryDescriptor)
    }

    private static func requireRegularFileOrMissing(
        directoryDescriptor: Int32,
        fileName: String,
        description: String
    ) throws {
        let descriptor = fileName.withCString { name in
            openat(
                directoryDescriptor,
                name,
                O_RDONLY | O_CLOEXEC | O_NOFOLLOW | O_NONBLOCK
            )
        }
        guard descriptor >= 0 else {
            if errno == ENOENT { return }
            throw MobileRemoteError(500, "\(description) inspection failed")
        }
        defer { _ = close(descriptor) }
        try requirePrivateRegularFile(
            descriptor: descriptor,
            message: "\(description) is unsafe"
        )
    }

    private static func requirePrivateRegularFile(
        descriptor: Int32,
        message: String
    ) throws {
        var metadata = stat()
        guard fstat(descriptor, &metadata) == 0,
              metadata.st_mode & mode_t(S_IFMT) == mode_t(S_IFREG),
              metadata.st_nlink == 1
        else {
            throw MobileRemoteError(500, message)
        }
    }

    private static func requireFileKind(
        descriptor: Int32,
        kind: mode_t,
        message: String
    ) throws {
        var metadata = stat()
        guard fstat(descriptor, &metadata) == 0,
              metadata.st_mode & mode_t(S_IFMT) == kind
        else {
            throw MobileRemoteError(500, message)
        }
    }

    private static func requireSafeFileName(_ fileName: String) throws {
        guard !fileName.isEmpty,
              fileName != ".",
              fileName != "..",
              !fileName.contains("/")
        else {
            throw MobileRemoteError(500, "device store filename is unsafe")
        }
    }

    /// Stable identity for this logical Host instance. The `macID` spelling
    /// is retained on the shipped wire, but the identity belongs to the Host,
    /// not specifically to mobile clients.
    static func defaultMacID() -> String {
        let url = LaunchConfig.unpeelDir
            .appendingPathComponent("mobile")
            .appendingPathComponent("mac-id")
        return stableHostID(at: url)
    }

    /// Return one durable Host identity even when multiple app processes
    /// sharing an UNPEEL_HOME launch for the first time together. The lock is
    /// cross-process; the second reader checks the file again only after the
    /// first writer has committed it.
    static func stableHostID(at url: URL) -> String {
        if let existing = persistedHostID(at: url) { return existing }

        do {
            try FileManager.default.createDirectory(
                at: url.deletingLastPathComponent(),
                withIntermediateDirectories: true
            )
        } catch {
            NSLog("[UnpeelNative] failed to create Host identity directory: \(error)")
            return UUID().uuidString.lowercased()
        }

        let lockURL = url.appendingPathExtension("lock")
        let fd = open(lockURL.path, O_CREAT | O_WRONLY, 0o600)
        guard fd >= 0 else {
            if let existing = persistedHostID(at: url) { return existing }
            let fallback = UUID().uuidString.lowercased()
            NSLog("[UnpeelNative] failed to lock Host identity file")
            return fallback
        }
        defer { close(fd) }
        guard flock(fd, LOCK_EX) == 0 else {
            if let existing = persistedHostID(at: url) { return existing }
            let fallback = UUID().uuidString.lowercased()
            NSLog("[UnpeelNative] failed to acquire Host identity lock")
            return fallback
        }
        defer { flock(fd, LOCK_UN) }

        if let existing = persistedHostID(at: url) { return existing }
        let id = UUID().uuidString.lowercased()
        do {
            try (id + "\n").write(to: url, atomically: true, encoding: .utf8)
            try FileManager.default.setAttributes(
                [.posixPermissions: 0o600],
                ofItemAtPath: url.path
            )
        } catch {
            NSLog("[UnpeelNative] failed to persist mobile mac id: \(error)")
        }
        return id
    }

    /// Read-only identity probe; also used by the workspace-gateway scope to
    /// pin a workspace's saved Host id without minting one for a
    /// never-started home.
    static func persistedHostID(at url: URL) -> String? {
        guard let raw = try? String(contentsOf: url, encoding: .utf8) else { return nil }
        let trimmed = raw.trimmingCharacters(in: .whitespacesAndNewlines)
        return trimmed.isEmpty ? nil : trimmed
    }

    static func bearerToken(from header: String?) -> String? {
        guard let header else { return nil }
        let trimmed = header.trimmingCharacters(in: .whitespacesAndNewlines)
        guard trimmed.lowercased().hasPrefix("bearer ") else { return nil }
        let token = trimmed.dropFirst("bearer ".count)
            .trimmingCharacters(in: .whitespacesAndNewlines)
        return token.isEmpty ? nil : token
    }

    static func sha256(_ value: String) -> String {
        SHA256.hash(data: Data(value.utf8))
            .map { String(format: "%02x", $0) }
            .joined()
    }

    static func randomToken(byteCount: Int) -> String {
        var bytes = [UInt8](repeating: 0, count: byteCount)
        let status = SecRandomCopyBytes(kSecRandomDefault, bytes.count, &bytes)
        if status != errSecSuccess {
            return (UUID().uuidString + UUID().uuidString)
                .replacingOccurrences(of: "-", with: "")
                .lowercased()
        }
        return Data(bytes).unpeelBase64URLString()
    }

    static func randomBytes(_ count: Int) -> Data {
        var bytes = [UInt8](repeating: 0, count: count)
        let status = SecRandomCopyBytes(kSecRandomDefault, bytes.count, &bytes)
        precondition(status == errSecSuccess, "SecRandomCopyBytes failed")
        return Data(bytes)
    }

    static func randomBase32Token(byteCount: Int) -> String {
        var bytes = [UInt8](repeating: 0, count: byteCount)
        let status = SecRandomCopyBytes(kSecRandomDefault, bytes.count, &bytes)
        if status != errSecSuccess {
            bytes = Array((UUID().uuidString + UUID().uuidString).utf8.prefix(byteCount))
        }
        let alphabet = Array("ABCDEFGHIJKLMNOPQRSTUVWXYZ234567")
        var output = ""
        var buffer = 0
        var bitsInBuffer = 0
        for byte in bytes {
            buffer = (buffer << 8) | Int(byte)
            bitsInBuffer += 8
            while bitsInBuffer >= 5 {
                bitsInBuffer -= 5
                output.append(alphabet[(buffer >> bitsInBuffer) & 0x1F])
            }
        }
        if bitsInBuffer > 0 {
            output.append(alphabet[(buffer << (5 - bitsInBuffer)) & 0x1F])
        }
        return output
    }

    static func unixMs(_ date: Date) -> Int64 {
        Int64(date.timeIntervalSince1970 * 1000)
    }
}

extension Notification.Name {
    /// Posted by `MobilePairingStore` whenever the paired-device set changes
    /// (pair, revoke, or credential rotation), from any thread.
    static let unpeelMobileDevicesChanged = Notification.Name(
        "unpeel.native.mobileDevicesChanged"
    )
}

private extension NSLock {
    func withLock<T>(_ operation: () throws -> T) rethrows -> T {
        lock()
        defer { unlock() }
        return try operation()
    }
}

private extension Data {
    func unpeelBase64URLString() -> String {
        base64EncodedString()
            .replacingOccurrences(of: "+", with: "-")
            .replacingOccurrences(of: "/", with: "_")
            .replacingOccurrences(of: "=", with: "")
    }
}
