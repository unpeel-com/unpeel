import XCTest
import UnpeelShared
@testable import UnpeelNative

private final class TestMobileE2EKeyStore: MobileE2EKeyStoring {
    var keys: [String: Data] = [:]
    var savedDeviceIDs: [String] = []
    var deletedDeviceIDs: [String] = []
    var onSave: ((Data, String) throws -> Void)?
    func load(deviceID: String) -> Data? { keys[deviceID] }
    func save(_ key: Data, deviceID: String) throws {
        keys[deviceID] = key
        savedDeviceIDs.append(deviceID)
        try onSave?(key, deviceID)
    }
    func delete(deviceID: String) {
        keys[deviceID] = nil
        deletedDeviceIDs.append(deviceID)
    }
}

final class MobilePairingStoreTests: XCTestCase {
    func testStableHostIDPersistsOneIdentityAcrossConcurrentFirstReads() async throws {
        let storageURL = tempURL()
        let directoryURL = storageURL.deletingLastPathComponent()
        defer { try? FileManager.default.removeItem(at: directoryURL) }

        let ids = await withTaskGroup(of: String.self, returning: [String].self) { group in
            for _ in 0..<32 {
                group.addTask {
                    MobilePairingStore.stableHostID(at: storageURL)
                }
            }

            var values: [String] = []
            for await value in group {
                values.append(value)
            }
            return values
        }

        let persisted = try String(contentsOf: storageURL, encoding: .utf8)
            .trimmingCharacters(in: .whitespacesAndNewlines)
        XCTAssertEqual(ids.count, 32)
        XCTAssertEqual(Set(ids), [persisted])
        XCTAssertFalse(persisted.isEmpty)
    }

    func testHostPairingPresentationConsumesOnlyItsMatchingActiveToken() throws {
        let firstPayload = RemotePairingPayload(
            macID: "host-1",
            macName: "Studio Mac",
            endpoint: try XCTUnwrap(URL(string: "http://127.0.0.1:17661/mobile")),
            token: "FIRST-TOKEN",
            expiresAtUnixMs: 1_000_000
        )
        let active = HostPairingPresentation.active(firstPayload)

        XCTAssertEqual(active.payload, firstPayload)
        XCTAssertEqual(active.code, RemotePairingCode.encode(firstPayload))
        XCTAssertFalse(active.completed)

        let completed = active.completing(token: firstPayload.token)
        XCTAssertNil(completed.payload)
        XCTAssertNil(completed.code)
        XCTAssertTrue(completed.completed)

        let newerPayload = RemotePairingPayload(
            macID: "host-1",
            macName: "Studio Mac",
            endpoint: firstPayload.endpoint,
            token: "NEWER-TOKEN",
            expiresAtUnixMs: 2_000_000
        )
        let newer = HostPairingPresentation.active(newerPayload)

        XCTAssertEqual(newer.completing(token: firstPayload.token), newer)
        XCTAssertEqual(newer.payload, newerPayload)
        XCTAssertNotNil(newer.code)
        XCTAssertFalse(newer.completed)
    }

    func testPairingIssuesBearerTokenAndStoresOnlyHash() throws {
        let storageURL = tempURL()
        let keyStore = TestMobileE2EKeyStore()
        let store = MobilePairingStore(
            storageURL: storageURL,
            macID: "mac-1",
            macName: "Studio Mac",
            e2eKeyStore: keyStore
        )
        let endpoint = URL(string: "http://192.168.1.20:49152/mobile")!
        let payload = store.beginPairing(
            endpoint: endpoint,
            now: Date(timeIntervalSince1970: 100)
        )

        let response = try store.pair(
            RemotePairingRequest(
                token: payload.token,
                device: .init(id: "phone-1", name: "iPhone", platform: "iOS", appVersion: "1")
            ),
            now: Date(timeIntervalSince1970: 110)
        )

        XCTAssertEqual(response.macID, "mac-1")
        XCTAssertEqual(response.endpoint, endpoint)
        XCTAssertEqual(response.deviceID, "phone-1")
        XCTAssertEqual(store.verifyAuthorizationHeader("Bearer \(response.authToken)"), "phone-1")
        XCTAssertEqual(store.devices.map(\.id), ["phone-1"])
        XCTAssertEqual(
            store.principalID(forDeviceID: "phone-1"),
            SessionOwnership.hostOwnerPrincipalID(hostID: "mac-1")
        )

        let persisted = try String(contentsOf: storageURL, encoding: .utf8)
        XCTAssertFalse(persisted.contains(response.authToken))
        XCTAssertTrue(persisted.contains(MobilePairingStore.sha256(response.authToken)))
        XCTAssertTrue(persisted.contains(#""principalID":"host-owner:mac-1""#))
        XCTAssertEqual(keyStore.keys["phone-1"]?.count, 32)
        let sharedKeys = try sharedE2EKeys(for: storageURL)
        let sharedValue = try XCTUnwrap(sharedKeys["mac-1.phone-1"])
        XCTAssertEqual(sharedKeys.count, 1)
        XCTAssertEqual(sharedValue.count, 44)
        XCTAssertTrue(sharedValue.hasSuffix("="))
        XCTAssertEqual(Data(base64Encoded: sharedValue), keyStore.keys["phone-1"])
        let attributes = try FileManager.default.attributesOfItem(atPath: storageURL.path)
        let permissions = try XCTUnwrap(attributes[.posixPermissions] as? NSNumber)
        XCTAssertEqual(permissions.intValue & 0o777, 0o600)
        let sharedAttributes = try FileManager.default.attributesOfItem(
            atPath: sharedE2EKeysURL(for: storageURL).path
        )
        let sharedPermissions = try XCTUnwrap(sharedAttributes[.posixPermissions] as? NSNumber)
        XCTAssertEqual(sharedPermissions.intValue & 0o777, 0o600)
    }

    func testWorkerCredentialCallbacksMirrorAndRemoveTheScopedKeychainKey() throws {
        let storageURL = tempURL()
        let directory = storageURL.deletingLastPathComponent()
        defer { try? FileManager.default.removeItem(at: directory) }
        let keyStore = TestMobileE2EKeyStore()
        let store = MobilePairingStore(
            storageURL: storageURL,
            macID: "mac-worker",
            macName: "Worker Mac",
            e2eKeyStore: keyStore
        )
        let endpoint = try XCTUnwrap(URL(string: "http://127.0.0.1:17661/mobile"))
        let payload = store.beginPairing(endpoint: endpoint)
        _ = try store.pair(RemotePairingRequest(
            token: payload.token,
            device: .init(id: "phone-worker", name: "iPhone", platform: "iOS")
        ))
        let shared = try XCTUnwrap(
            Data(base64Encoded: sharedE2EKeys(for: storageURL)["mac-worker.phone-worker"] ?? "")
        )

        // The Rust worker's shared revision wins and is copied into the
        // scoped Keychain account on its connection-scoped sync callback.
        keyStore.keys["phone-worker"] = nil
        keyStore.savedDeviceIDs.removeAll()
        try store.reconcilePlatformE2EKeys()
        XCTAssertEqual(keyStore.keys["phone-worker"], shared)
        XCTAssertEqual(keyStore.savedDeviceIDs, ["phone-worker"])

        // Simulate the worker's authority-first revoke. The callback may
        // remove Keychain only after a fresh locked read proves the device is
        // no longer authorized.
        var authority = try XCTUnwrap(
            JSONSerialization.jsonObject(with: Data(contentsOf: storageURL))
                as? [String: Any]
        )
        authority["devices"] = []
        try JSONSerialization.data(withJSONObject: authority)
            .write(to: storageURL, options: .atomic)
        try store.removePlatformE2EKey(deviceID: "phone-worker")
        XCTAssertNil(keyStore.keys["phone-worker"])
        XCTAssertEqual(keyStore.deletedDeviceIDs, ["phone-worker"])
    }

    func testPushTokenRegistrationIsLockedTypedAndRejectsUnknownDevices() throws {
        let storageURL = tempURL()
        let directoryURL = storageURL.deletingLastPathComponent()
        defer { try? FileManager.default.removeItem(at: directoryURL) }
        let store = MobilePairingStore(
            storageURL: storageURL,
            macID: "mac-push",
            macName: "Push Mac",
            e2eKeyStore: TestMobileE2EKeyStore()
        )
        let endpoint = try XCTUnwrap(URL(string: "http://127.0.0.1:17661/mobile"))
        let payload = store.beginPairing(endpoint: endpoint)
        _ = try store.pair(RemotePairingRequest(
            token: payload.token,
            device: .init(id: "phone-push", name: "iPhone", platform: "iOS")
        ))

        XCTAssertNil(try store.setPushToken(
            deviceID: "missing",
            token: "0011223344556677",
            environment: "sandbox"
        ))
        XCTAssertEqual(try store.setPushToken(
            deviceID: "phone-push",
            token: "0011223344556677",
            environment: "sandbox"
        ), true)
        XCTAssertEqual(try store.setPushToken(
            deviceID: "phone-push",
            token: "0011223344556677",
            environment: "sandbox"
        ), false)
        XCTAssertEqual(store.pushTargets().count, 1)
        XCTAssertEqual(store.pushTargets().first?.deviceID, "phone-push")
        XCTAssertEqual(store.pushTargets().first?.token, "0011223344556677")
        XCTAssertEqual(store.pushTargets().first?.environment, "sandbox")

        let persisted = try String(contentsOf: storageURL, encoding: .utf8)
        XCTAssertTrue(persisted.contains(#""apnsToken":"0011223344556677""#))
        XCTAssertTrue(persisted.contains(#""apnsEnvironment":"sandbox""#))
    }

    func testControllerAssistedPairingReturnsTheHostsDirectEndpoint() throws {
        let storageURL = tempURL()
        let store = MobilePairingStore(
            storageURL: storageURL,
            macID: "remote-host",
            macName: "Upstash Host",
            e2eKeyStore: TestMobileE2EKeyStore()
        )
        let proxy = try XCTUnwrap(URL(
            string: "http://192.168.1.20:49152/mobile/pairing-proxy/INVITE-1"
        ))
        let direct = try XCTUnwrap(URL(string: "http://10.0.0.8:17661/mobile"))
        let payload = store.beginPairing(endpoint: proxy, directEndpoint: direct)
        let request = RemotePairingRequest(
            token: payload.token,
            device: .init(id: "phone-remote", name: "iPhone", platform: "iOS")
        )
        let envelope = try RemotePairingCrypto.seal(
            JSONEncoder().encode(request),
            token: payload.token,
            macID: payload.macID,
            endpoint: payload.endpoint,
            direction: .request
        )

        XCTAssertEqual(try store.decryptPairingRequest(envelope), request)
        let response = try store.pair(request)
        XCTAssertEqual(response.endpoint, proxy)
        XCTAssertEqual(response.directEndpoint, direct)
        XCTAssertEqual(
            store.verifyAuthorizationHeader("Bearer \(response.authToken)"),
            "phone-remote"
        )
    }

    func testExistingLegacyRecordMigratesWithoutRepairAndSharedRevisionWins() throws {
        let storageURL = tempURL()
        let directory = storageURL.deletingLastPathComponent()
        defer { try? FileManager.default.removeItem(at: directory) }
        try FileManager.default.createDirectory(at: directory, withIntermediateDirectories: true)

        // This is the pre-relayAllowed record shape already on customer Macs.
        let authority = try JSONSerialization.data(withJSONObject: [
            "version": 1,
            "devices": [[
                "id": "phone-legacy",
                "name": "Existing iPhone",
                "platform": "iOS",
                "appVersion": "0.1.0",
                "tokenHash": String(repeating: "a", count: 64),
                "pairedAtUnixMs": 1_700_000_000_000 as Int64,
                "relayTokenHash": String(repeating: "b", count: 64),
            ]],
        ], options: [.sortedKeys])
        try authority.write(to: storageURL)
        let originalAuthority = try Data(contentsOf: storageURL)

        let keyStore = TestMobileE2EKeyStore()
        let legacyKey = Data((0..<32).map(UInt8.init))
        keyStore.keys["phone-legacy"] = legacyKey

        let migrated = MobilePairingStore(
            storageURL: storageURL,
            macID: "mac-existing",
            macName: "Studio Mac",
            e2eKeyStore: keyStore
        )

        XCTAssertEqual(migrated.devices.map(\.id), ["phone-legacy"])
        XCTAssertEqual(
            migrated.principalID(forDeviceID: "phone-legacy"),
            SessionOwnership.hostOwnerPrincipalID(hostID: "mac-existing")
        )
        XCTAssertEqual(try Data(contentsOf: storageURL), originalAuthority)
        XCTAssertEqual(keyStore.keys["phone-legacy"], legacyKey)
        XCTAssertTrue(keyStore.deletedDeviceIDs.isEmpty, "migration must preserve Keychain copies")
        let firstMap = try sharedE2EKeys(for: storageURL)
        XCTAssertEqual(firstMap, [
            "mac-existing.phone-legacy": legacyKey.base64EncodedString(),
        ])
        let firstMode = try XCTUnwrap(
            FileManager.default.attributesOfItem(
                atPath: sharedE2EKeysURL(for: storageURL).path
            )[.posixPermissions] as? NSNumber
        )
        XCTAssertEqual(firstMode.intValue & 0o777, 0o600)

        // A TUI rotation is authoritative. A later native read copies that
        // exact shared revision back into the scoped Keychain account.
        let tuiRotatedKey = Data((32..<64).map(UInt8.init))
        let rotatedMap = [
            "mac-existing.phone-legacy": tuiRotatedKey.base64EncodedString(),
        ]
        try JSONSerialization.data(withJSONObject: rotatedMap, options: [.sortedKeys])
            .write(to: sharedE2EKeysURL(for: storageURL), options: .atomic)
        try FileManager.default.setAttributes(
            [.posixPermissions: NSNumber(value: 0o600)],
            ofItemAtPath: sharedE2EKeysURL(for: storageURL).path
        )

        XCTAssertEqual(migrated.e2eKey(forDeviceID: "phone-legacy"), tuiRotatedKey)
        XCTAssertEqual(keyStore.keys["phone-legacy"], tuiRotatedKey)
        XCTAssertTrue(keyStore.deletedDeviceIDs.isEmpty)
        XCTAssertEqual(try Data(contentsOf: storageURL), originalAuthority)
    }

    func testExistingPrivateStoresAreCorrectedToMode0600WithoutCredentialRewrite() throws {
        let storageURL = tempURL()
        let directory = storageURL.deletingLastPathComponent()
        defer { try? FileManager.default.removeItem(at: directory) }
        try FileManager.default.createDirectory(at: directory, withIntermediateDirectories: true)
        try legacyAuthority(deviceID: "phone-1").write(to: storageURL)

        let existingKey = Data(repeating: 4, count: 32)
        let sharedURL = sharedE2EKeysURL(for: storageURL)
        try JSONSerialization.data(withJSONObject: [
            "mac-1.phone-1": existingKey.base64EncodedString(),
        ]).write(to: sharedURL)
        try FileManager.default.setAttributes(
            [.posixPermissions: NSNumber(value: 0o644)],
            ofItemAtPath: storageURL.path
        )
        try FileManager.default.setAttributes(
            [.posixPermissions: NSNumber(value: 0o644)],
            ofItemAtPath: sharedURL.path
        )

        let keyStore = TestMobileE2EKeyStore()
        keyStore.keys["phone-1"] = existingKey
        let store = MobilePairingStore(
            storageURL: storageURL,
            macID: "mac-1",
            macName: "Studio Mac",
            e2eKeyStore: keyStore
        )

        XCTAssertEqual(store.devices.map(\.id), ["phone-1"])
        XCTAssertTrue(keyStore.savedDeviceIDs.isEmpty, "equal revisions must not rewrite Keychain")
        XCTAssertEqual(try posixMode(at: storageURL), 0o600)
        XCTAssertEqual(try posixMode(at: sharedURL), 0o600)
        XCTAssertEqual(
            try sharedE2EKeys(for: storageURL),
            ["mac-1.phone-1": existingKey.base64EncodedString()]
        )
    }

    func testSharedSaveFailureAfterKeychainMutationRollsKeychainBack() throws {
        let root = FileManager.default.temporaryDirectory
            .appendingPathComponent("unpeel-mobile-e2e-save-failure-\(UUID().uuidString)")
        let directory = root.appendingPathComponent("mobile")
        let storageURL = directory.appendingPathComponent("devices.json")
        let outside = root.appendingPathComponent("outside.json")
        defer { try? FileManager.default.removeItem(at: root) }
        try FileManager.default.createDirectory(at: directory, withIntermediateDirectories: true)
        let sentinel = Data("shared-target-must-not-change".utf8)
        try sentinel.write(to: outside)

        let keyStore = TestMobileE2EKeyStore()
        var installedFailure = false
        keyStore.onSave = { _, _ in
            guard !installedFailure else { return }
            installedFailure = true
            try FileManager.default.createSymbolicLink(
                at: self.sharedE2EKeysURL(for: storageURL),
                withDestinationURL: outside
            )
        }
        let store = MobilePairingStore(
            storageURL: storageURL,
            macID: "mac-1",
            macName: "Studio Mac",
            e2eKeyStore: keyStore
        )
        let payload = store.beginPairing(
            endpoint: URL(string: "http://192.168.1.20:49152/mobile")!
        )

        XCTAssertThrowsError(try store.pair(RemotePairingRequest(
            token: payload.token,
            device: .init(id: "phone-1", name: "iPhone", platform: "iOS")
        )))

        XCTAssertTrue(installedFailure)
        XCTAssertEqual(keyStore.savedDeviceIDs, ["phone-1"])
        XCTAssertEqual(keyStore.deletedDeviceIDs, ["phone-1"])
        XCTAssertNil(keyStore.keys["phone-1"])
        XCTAssertTrue(store.devices.isEmpty)
        XCTAssertFalse(FileManager.default.fileExists(atPath: storageURL.path))
        XCTAssertEqual(try Data(contentsOf: outside), sentinel)
        XCTAssertNotNil(try? FileManager.default.destinationOfSymbolicLink(
            atPath: sharedE2EKeysURL(for: storageURL).path
        ))
    }

    func testSharedE2ERegistrySymlinkFailsClosedWithoutChangingKeychainOrTarget() throws {
        let root = FileManager.default.temporaryDirectory
            .appendingPathComponent("unpeel-mobile-e2e-symlink-\(UUID().uuidString)")
        let directory = root.appendingPathComponent("mobile")
        let storageURL = directory.appendingPathComponent("devices.json")
        let outside = root.appendingPathComponent("outside.json")
        defer { try? FileManager.default.removeItem(at: root) }
        try FileManager.default.createDirectory(at: directory, withIntermediateDirectories: true)
        let directToken = "still-valid-over-lan"
        try legacyAuthority(
            deviceID: "phone-1",
            tokenHash: MobilePairingStore.sha256(directToken)
        ).write(to: storageURL)
        let sentinel = Data("do-not-touch".utf8)
        try sentinel.write(to: outside)
        try FileManager.default.createSymbolicLink(
            at: sharedE2EKeysURL(for: storageURL),
            withDestinationURL: outside
        )

        let keyStore = TestMobileE2EKeyStore()
        let oldKey = Data(repeating: 7, count: 32)
        keyStore.keys["phone-1"] = oldKey
        let store = MobilePairingStore(
            storageURL: storageURL,
            macID: "mac-1",
            macName: "Studio Mac",
            e2eKeyStore: keyStore
        )

        // The Link-only store fails closed without taking Direct authority or
        // the user's ability to revoke the device down with it.
        XCTAssertEqual(store.devices.map(\.id), ["phone-1"])
        XCTAssertEqual(
            store.verifyAuthorizationHeader("Bearer \(directToken)"),
            "phone-1"
        )
        XCTAssertNil(store.e2eKey(forDeviceID: "phone-1"))
        XCTAssertEqual(keyStore.keys["phone-1"], oldKey)
        XCTAssertTrue(keyStore.savedDeviceIDs.isEmpty)
        XCTAssertTrue(keyStore.deletedDeviceIDs.isEmpty)
        XCTAssertEqual(try Data(contentsOf: outside), sentinel)

        store.setRelayAllowed(deviceID: "phone-1", allowed: false)
        XCTAssertEqual(store.devices.first?.relayAllowed, false)
        XCTAssertEqual(try Data(contentsOf: outside), sentinel)
        XCTAssertTrue(keyStore.deletedDeviceIDs.isEmpty)

        XCTAssertTrue(store.revokeDevice(id: "phone-1"))
        XCTAssertTrue(store.devices.isEmpty)
        XCTAssertNil(keyStore.keys["phone-1"])
        XCTAssertEqual(keyStore.deletedDeviceIDs, ["phone-1"])
        XCTAssertEqual(try Data(contentsOf: outside), sentinel)
        let persisted = try XCTUnwrap(
            JSONSerialization.jsonObject(with: Data(contentsOf: storageURL))
                as? [String: Any]
        )
        XCTAssertTrue((persisted["devices"] as? [[String: Any]])?.isEmpty == true)
        let destination = try FileManager.default.destinationOfSymbolicLink(
            atPath: sharedE2EKeysURL(for: storageURL).path
        )
        XCTAssertEqual(URL(fileURLWithPath: destination).standardizedFileURL, outside.standardizedFileURL)
    }

    func testInvalidSharedE2ERevisionFailsClosedInsteadOfFallingBackToKeychain() throws {
        let storageURL = tempURL()
        let directory = storageURL.deletingLastPathComponent()
        defer { try? FileManager.default.removeItem(at: directory) }
        try FileManager.default.createDirectory(at: directory, withIntermediateDirectories: true)
        let directToken = "valid-direct-token"
        try legacyAuthority(
            deviceID: "phone-1",
            tokenHash: MobilePairingStore.sha256(directToken)
        ).write(to: storageURL)
        let invalidValue = Data(repeating: 9, count: 31).base64EncodedString()
        let invalidMap = ["mac-1.phone-1": invalidValue]
        try JSONSerialization.data(withJSONObject: invalidMap)
            .write(to: sharedE2EKeysURL(for: storageURL))

        let keyStore = TestMobileE2EKeyStore()
        let keychainValue = Data(repeating: 7, count: 32)
        keyStore.keys["phone-1"] = keychainValue
        let store = MobilePairingStore(
            storageURL: storageURL,
            macID: "mac-1",
            macName: "Studio Mac",
            e2eKeyStore: keyStore
        )

        XCTAssertEqual(store.devices.map(\.id), ["phone-1"])
        XCTAssertEqual(
            store.verifyAuthorizationHeader("Bearer \(directToken)"),
            "phone-1"
        )
        XCTAssertNil(store.e2eKey(forDeviceID: "phone-1"))
        XCTAssertEqual(keyStore.keys["phone-1"], keychainValue)
        XCTAssertTrue(keyStore.savedDeviceIDs.isEmpty)
        XCTAssertTrue(keyStore.deletedDeviceIDs.isEmpty)
        XCTAssertEqual(try sharedE2EKeys(for: storageURL), invalidMap)
    }

    func testFailedAuthorityCommitRollsBackSharedAndKeychainCredentialRevision() throws {
        let root = FileManager.default.temporaryDirectory
            .appendingPathComponent("unpeel-mobile-e2e-rollback-\(UUID().uuidString)")
        let directory = root.appendingPathComponent("mobile")
        let storageURL = directory.appendingPathComponent("devices.json")
        let outside = root.appendingPathComponent("outside.json")
        defer { try? FileManager.default.removeItem(at: root) }
        try FileManager.default.createDirectory(at: directory, withIntermediateDirectories: true)

        let keyStore = TestMobileE2EKeyStore()
        let store = MobilePairingStore(
            storageURL: storageURL,
            macID: "mac-1",
            macName: "Studio Mac",
            e2eKeyStore: keyStore
        )
        let endpoint = URL(string: "http://192.168.1.20:49152/mobile")!
        let first = store.beginPairing(endpoint: endpoint)
        _ = try store.pair(RemotePairingRequest(
            token: first.token,
            device: .init(id: "phone-1", name: "iPhone", platform: "iOS")
        ))
        let previousKey = try XCTUnwrap(keyStore.keys["phone-1"])
        let previousShared = try XCTUnwrap(
            sharedE2EKeys(for: storageURL)["mac-1.phone-1"]
        )
        let sentinel = Data("authority-target-must-not-change".utf8)
        try sentinel.write(to: outside)

        var replacedAuthority = false
        keyStore.onSave = { key, _ in
            guard key != previousKey, !replacedAuthority else { return }
            replacedAuthority = true
            try FileManager.default.removeItem(at: storageURL)
            try FileManager.default.createSymbolicLink(
                at: storageURL,
                withDestinationURL: outside
            )
        }

        let second = store.beginPairing(endpoint: endpoint)
        XCTAssertThrowsError(try store.pair(RemotePairingRequest(
            token: second.token,
            device: .init(id: "phone-1", name: "iPhone", platform: "iOS")
        )))

        XCTAssertTrue(replacedAuthority)
        XCTAssertEqual(keyStore.keys["phone-1"], previousKey)
        XCTAssertEqual(
            try sharedE2EKeys(for: storageURL)["mac-1.phone-1"],
            previousShared
        )
        XCTAssertEqual(try Data(contentsOf: outside), sentinel)
    }

    func testPairingCodeIsOneTimeAndExpires() throws {
        let store = MobilePairingStore(
            storageURL: tempURL(),
            macID: "mac-1",
            macName: "Studio Mac",
            e2eKeyStore: TestMobileE2EKeyStore()
        )
        let endpoint = URL(string: "http://192.168.1.20:49152/mobile")!
        let payload = store.beginPairing(
            endpoint: endpoint,
            now: Date(timeIntervalSince1970: 100),
            ttlSeconds: 5
        )

        XCTAssertThrowsError(try store.pair(
            RemotePairingRequest(
                token: payload.token,
                device: .init(id: "late-phone", name: "Late", platform: "iOS")
            ),
            now: Date(timeIntervalSince1970: 106)
        ))

        let fresh = store.beginPairing(endpoint: endpoint, now: Date(timeIntervalSince1970: 200))
        _ = try store.pair(
            RemotePairingRequest(
                token: fresh.token,
                device: .init(id: "phone-1", name: "iPhone", platform: "iOS")
            ),
            now: Date(timeIntervalSince1970: 201)
        )
        XCTAssertThrowsError(try store.pair(
            RemotePairingRequest(
                token: fresh.token,
                device: .init(id: "phone-2", name: "Other", platform: "iOS")
            ),
            now: Date(timeIntervalSince1970: 202)
        ))
    }

    func testEncryptedPairingRequestAuthenticatesScannedMacAndEndpoint() throws {
        let store = MobilePairingStore(
            storageURL: tempURL(),
            macID: "mac-secure-1",
            macName: "Studio Mac",
            e2eKeyStore: TestMobileE2EKeyStore()
        )
        let endpoint = URL(string: "http://192.168.1.20:49152/mobile")!
        let payload = store.beginPairing(endpoint: endpoint)
        let request = RemotePairingRequest(
            token: payload.token,
            device: .init(id: "phone-1", name: "iPhone", platform: "iOS")
        )
        let plaintext = try JSONEncoder().encode(request)
        let envelope = try RemotePairingCrypto.seal(
            plaintext,
            token: payload.token,
            macID: payload.macID,
            endpoint: payload.endpoint,
            direction: .request
        )
        XCTAssertEqual(try store.decryptPairingRequest(envelope), request)

        let wrongContext = try RemotePairingCrypto.seal(
            plaintext,
            token: payload.token,
            macID: "mac-attacker",
            endpoint: payload.endpoint,
            direction: .request
        )
        XCTAssertThrowsError(try store.decryptPairingRequest(wrongContext))
    }

    func testRevokeInvalidatesDeviceToken() throws {
        let storageURL = tempURL()
        let keyStore = TestMobileE2EKeyStore()
        let store = MobilePairingStore(
            storageURL: storageURL,
            macID: "mac-1",
            macName: "Studio Mac",
            e2eKeyStore: keyStore
        )
        let endpoint = URL(string: "http://192.168.1.20:49152/mobile")!
        let payload = store.beginPairing(endpoint: endpoint)
        let response = try store.pair(
            RemotePairingRequest(
                token: payload.token,
                device: .init(id: "phone-1", name: "iPhone", platform: "iOS")
            )
        )

        XCTAssertEqual(store.verifyAuthorizationHeader("Bearer \(response.authToken)"), "phone-1")
        let pairedKey = try XCTUnwrap(keyStore.keys["phone-1"])
        let rotated = try XCTUnwrap(store.rotateRelayCredentials(deviceID: "phone-1"))
        let rotatedKey = try XCTUnwrap(rotated.e2eKey)
        XCTAssertNotEqual(rotatedKey, pairedKey)
        XCTAssertEqual(keyStore.keys["phone-1"], rotatedKey)
        XCTAssertEqual(
            try sharedE2EKeys(for: storageURL)["mac-1.phone-1"],
            rotatedKey.base64EncodedString()
        )
        store.revokeDevice(id: "phone-1")
        XCTAssertNil(store.verifyAuthorizationHeader("Bearer \(response.authToken)"))
        XCTAssertTrue(store.devices.isEmpty)
        XCTAssertNil(try sharedE2EKeys(for: storageURL)["mac-1.phone-1"])
        XCTAssertNil(keyStore.keys["phone-1"])
        XCTAssertEqual(keyStore.deletedDeviceIDs, ["phone-1"])
    }

    func testExternalRevocationCannotBeResurrectedByNativeMetadataWrite() throws {
        let storageURL = tempURL()
        let store = MobilePairingStore(
            storageURL: storageURL,
            macID: "mac-1",
            macName: "Studio Mac",
            e2eKeyStore: TestMobileE2EKeyStore()
        )
        let endpoint = URL(string: "http://192.168.1.20:49152/mobile")!

        let first = store.beginPairing(endpoint: endpoint)
        let deviceA = try store.pair(
            RemotePairingRequest(
                token: first.token,
                device: .init(id: "phone-a", name: "A", platform: "iOS")
            ),
            now: Date(timeIntervalSince1970: 10)
        )
        let second = store.beginPairing(endpoint: endpoint)
        let deviceB = try store.pair(
            RemotePairingRequest(
                token: second.token,
                device: .init(id: "phone-b", name: "B", platform: "iOS")
            ),
            now: Date(timeIntervalSince1970: 20)
        )

        // Simulate the TUI's atomic revoke while this native store still has
        // its old A+B display cache.
        let data = try Data(contentsOf: storageURL)
        var json = try XCTUnwrap(
            JSONSerialization.jsonObject(with: data) as? [String: Any]
        )
        var devices = try XCTUnwrap(json["devices"] as? [[String: Any]])
        devices.removeAll { $0["id"] as? String == "phone-a" }
        json["devices"] = devices
        try JSONSerialization.data(withJSONObject: json)
            .write(to: storageURL, options: .atomic)

        // Updating B's lastSeen used to serialize stale [A,B] and restore A.
        XCTAssertEqual(
            store.verifyAuthorizationHeader(
                "Bearer \(deviceB.authToken)",
                now: Date(timeIntervalSince1970: 200)
            ),
            "phone-b"
        )
        XCTAssertNil(store.verifyAuthorizationHeader("Bearer \(deviceA.authToken)"))
        XCTAssertEqual(store.devices.map(\.id), ["phone-b"])

        let persisted = try JSONSerialization.jsonObject(
            with: Data(contentsOf: storageURL)
        ) as? [String: Any]
        let persistedDevices = persisted?["devices"] as? [[String: Any]]
        XCTAssertEqual(persistedDevices?.compactMap { $0["id"] as? String }, ["phone-b"])
    }

    func testRelayAuthorizationRevisionReloadsExternalRotationAndScopeChange() throws {
        let storageURL = tempURL()
        let store = MobilePairingStore(
            storageURL: storageURL,
            macID: "mac-1",
            macName: "Studio Mac",
            e2eKeyStore: TestMobileE2EKeyStore()
        )
        let payload = store.beginPairing(
            endpoint: URL(string: "http://192.168.1.20:49152/mobile")!
        )
        _ = try store.pair(
            RemotePairingRequest(
                token: payload.token,
                device: .init(id: "phone-1", name: "iPhone", platform: "iOS")
            )
        )
        let original = try XCTUnwrap(store.relayTokenHash(forDeviceID: "phone-1"))
        XCTAssertEqual(original.count, 64)

        // Simulate a credential rotation by the TUI. A live Link session is
        // leased to the exact old hash, so the native reader must observe the
        // replacement even without an in-process notification.
        var json = try XCTUnwrap(
            JSONSerialization.jsonObject(with: Data(contentsOf: storageURL))
                as? [String: Any]
        )
        var devices = try XCTUnwrap(json["devices"] as? [[String: Any]])
        let rotated = String(repeating: "a", count: 64)
        devices[0]["relayTokenHash"] = rotated
        json["devices"] = devices
        try JSONSerialization.data(withJSONObject: json)
            .write(to: storageURL, options: .atomic)
        XCTAssertEqual(store.relayTokenHash(forDeviceID: "phone-1"), rotated)

        devices[0]["relayAllowed"] = false
        json["devices"] = devices
        try JSONSerialization.data(withJSONObject: json)
            .write(to: storageURL, options: .atomic)
        XCTAssertNil(store.relayTokenHash(forDeviceID: "phone-1"))
    }

    func testBearerParserRequiresAuthorizationScheme() {
        XCTAssertEqual(MobilePairingStore.bearerToken(from: "Bearer abc123"), "abc123")
        XCTAssertNil(MobilePairingStore.bearerToken(from: "abc123"))
        XCTAssertNil(MobilePairingStore.bearerToken(from: "  "))
    }

    private func tempURL() -> URL {
        FileManager.default.temporaryDirectory
            .appendingPathComponent("unpeel-mobile-pairing-\(UUID().uuidString)")
            .appendingPathComponent("devices.json")
    }

    private func sharedE2EKeysURL(for storageURL: URL) -> URL {
        storageURL.deletingLastPathComponent().appendingPathComponent("e2e-keys.json")
    }

    private func sharedE2EKeys(for storageURL: URL) throws -> [String: String] {
        try JSONDecoder().decode(
            [String: String].self,
            from: Data(contentsOf: sharedE2EKeysURL(for: storageURL))
        )
    }

    private func posixMode(at url: URL) throws -> Int {
        let attributes = try FileManager.default.attributesOfItem(atPath: url.path)
        return try XCTUnwrap(attributes[.posixPermissions] as? NSNumber).intValue & 0o777
    }

    private func legacyAuthority(
        deviceID: String,
        tokenHash: String = String(repeating: "a", count: 64)
    ) throws -> Data {
        try JSONSerialization.data(withJSONObject: [
            "version": 1,
            "devices": [[
                "id": deviceID,
                "name": "Existing iPhone",
                "platform": "iOS",
                "appVersion": "0.1.0",
                "tokenHash": tokenHash,
                "pairedAtUnixMs": 1_700_000_000_000 as Int64,
                "relayTokenHash": String(repeating: "b", count: 64),
            ]],
        ], options: [.sortedKeys])
    }
}
