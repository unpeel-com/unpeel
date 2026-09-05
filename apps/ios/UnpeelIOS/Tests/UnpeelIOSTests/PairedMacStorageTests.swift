import XCTest
import Security
import UnpeelShared
@testable import UnpeelIOS

/// Multi-Mac storage invariants: the pure collection helpers and the
/// one-time migration from the single-Mac scheme. The migration runs against
/// a scratch UserDefaults suite and a dictionary-backed fake keychain —
/// `swift test` executes on macOS, where the real RemoteKeychain would hit
/// the login keychain.
@MainActor
final class PairedMacStorageTests: XCTestCase {
    private var defaults: UserDefaults!
    private var suiteName: String!

    private let legacyRecordKey = "unpeel.ios.pairedMac"
    private let recordsKey = "unpeel.ios.pairedMacs"
    private let activeMacIDKey = "unpeel.ios.activeMacID"
    private let deviceIDKey = "unpeel.ios.deviceID"

    override func setUp() {
        super.setUp()
        suiteName = "test.\(UUID().uuidString)"
        defaults = UserDefaults(suiteName: suiteName)
    }

    override func tearDown() {
        defaults.removePersistentDomain(forName: suiteName)
        super.tearDown()
    }

    private func record(
        macID: String,
        name: String = "Mac",
        endpoint: String = "http://192.168.1.10:4485"
    ) -> PairedMacRecord {
        PairedMacRecord(
            macID: macID,
            macName: name,
            endpoint: URL(string: endpoint)!,
            deviceID: "device-1",
            pairedAtUnixMs: 1
        )
    }

    private func storedRecords() -> [PairedMacRecord] {
        guard let data = defaults.data(forKey: recordsKey) else { return [] }
        return (try? JSONDecoder().decode([PairedMacRecord].self, from: data)) ?? []
    }

    // MARK: - PairedMacCollection

    func testUpsertingAppendsNewMac() {
        let records = [record(macID: "a")]
        let out = PairedMacCollection.upserting(records, with: record(macID: "b"))
        XCTAssertEqual(out.map(\.macID), ["a", "b"])
    }

    func testUpsertingReplacesInPlacePreservingOrder() {
        let records = [record(macID: "a"), record(macID: "b"), record(macID: "c")]
        let updated = record(macID: "b", name: "Renamed", endpoint: "http://10.0.0.9:1234")
        let out = PairedMacCollection.upserting(records, with: updated)
        XCTAssertEqual(out.map(\.macID), ["a", "b", "c"])
        XCTAssertEqual(out[1].macName, "Renamed")
        XCTAssertEqual(out[1].endpoint.absoluteString, "http://10.0.0.9:1234")
    }

    func testRemovingDropsOnlyThatMac() {
        let records = [record(macID: "a"), record(macID: "b")]
        XCTAssertEqual(PairedMacCollection.removing(records, macID: "a").map(\.macID), ["b"])
        XCTAssertEqual(PairedMacCollection.removing(records, macID: "missing").map(\.macID), ["a", "b"])
        XCTAssertTrue(
            PairedMacCollection.removing([record(macID: "a")], macID: "a").isEmpty
        )
    }

    func testRecordArrayRoundTrips() throws {
        let records = [record(macID: "a"), record(macID: "b", name: "Studio")]
        let data = try JSONEncoder().encode(records)
        let decoded = try JSONDecoder().decode([PairedMacRecord].self, from: data)
        XCTAssertEqual(decoded, records)
    }

    // MARK: - Protected Keychain hydration

    func testHydrationPrunesOnlyExplicitlyMissingToken() {
        let missing = record(macID: "mac-missing", name: "Missing")
        let retained = record(macID: "mac-retained", name: "Retained")

        let hydration = PairedMacHydration.resolve(
            records: [missing, retained],
            preferredActiveMacID: missing.macID,
            readToken: {
                $0 == missing.macID ? .notFound : .found("bearer-retained")
            }
        )

        XCTAssertEqual(hydration.records, [retained])
        XCTAssertEqual(hydration.activeRecord, retained)
        XCTAssertEqual(hydration.activeToken, "bearer-retained")
        XCTAssertFalse(hydration.isTemporarilyUnavailable)
    }

    func testInteractionNotAllowedPreservesMixedMacRecordsActiveAndDeviceIDs() {
        let available = record(macID: "mac-a", name: "Available")
        var protected = record(macID: "mac-b", name: "Protected")
        protected.deviceID = "stable-host-device-id"
        defaults.set(protected.macID, forKey: activeMacIDKey)
        defaults.set("stable-phone-id", forKey: deviceIDKey)

        let hydration = PairedMacHydration.resolve(
            records: [available, protected],
            preferredActiveMacID: defaults.string(forKey: activeMacIDKey),
            readToken: {
                $0 == protected.macID
                    ? .temporarilyUnavailable(errSecInteractionNotAllowed)
                    : .found("bearer-a")
            }
        )

        XCTAssertEqual(hydration.records, [available, protected])
        XCTAssertEqual(hydration.activeRecord, protected)
        XCTAssertNil(hydration.activeToken)
        XCTAssertEqual(hydration.unavailableStatuses, [errSecInteractionNotAllowed])
        XCTAssertEqual(defaults.string(forKey: activeMacIDKey), protected.macID)
        XCTAssertEqual(defaults.string(forKey: deviceIDKey), "stable-phone-id")
        XCTAssertEqual(hydration.activeRecord?.deviceID, "stable-host-device-id")
    }

    func testUnavailableThenFoundRehydratesSamePairingAsNewDirectGeneration() throws {
        var saved = record(macID: "mac-1", name: "Build 12 Mac")
        saved.deviceID = "stable-host-device-id"
        let unavailable = PairedMacHydration.resolve(
            records: [saved],
            preferredActiveMacID: saved.macID,
            readToken: { _ in .temporarilyUnavailable(errSecInteractionNotAllowed) }
        )

        XCTAssertEqual(unavailable.records, [saved])
        XCTAssertEqual(unavailable.activeRecord, saved)
        XCTAssertNil(
            PairedMacHydration.directActivation(from: unavailable, currentEpoch: 14),
            "a protected bearer must not create a client before unlock"
        )

        // Model the foreground/protected-data retry. This consumes the same
        // persisted record and bearer slot; no QR/pairing response participates.
        let found = PairedMacHydration.resolve(
            records: unavailable.records,
            preferredActiveMacID: unavailable.activeRecord?.macID,
            readToken: { _ in .found("bearer-after-unlock") }
        )
        let activation = try XCTUnwrap(
            PairedMacHydration.directActivation(from: found, currentEpoch: 14)
        )

        XCTAssertEqual(activation.record, saved)
        XCTAssertEqual(activation.record.deviceID, "stable-host-device-id")
        XCTAssertEqual(activation.client.baseURL, saved.endpoint)
        XCTAssertEqual(activation.client.authToken, "bearer-after-unlock")
        XCTAssertFalse(activation.client.isRelay)
        XCTAssertEqual(activation.epoch, 15)
    }

    func testEmptyBearerNeverActivatesOrPrunesStableRecord() {
        let saved = record(macID: "mac-1")
        let hydration = PairedMacHydration.resolve(
            records: [saved],
            preferredActiveMacID: saved.macID,
            readToken: { _ in .found("") }
        )

        XCTAssertEqual(hydration.records, [saved])
        XCTAssertEqual(hydration.activeRecord, saved)
        XCTAssertNil(hydration.activeToken)
        XCTAssertNil(PairedMacHydration.directActivation(from: hydration, currentEpoch: 2))
    }

    // MARK: - Migration

    private struct FakeKeychain {
        var tokens: [String: String] = [:]           // macID → token
        var relay: [String: RelayCredentials] = [:]  // macID → credentials
        var legacyToken: String?
        var legacyRelay: RelayCredentials?
    }

    private func runMigration(
        _ keychain: inout FakeKeychain,
        tokenWriteSucceeds: Bool = true,
        relayWriteSucceeds: Bool = true
    ) {
        // `inout` can't be captured by escaping-looking closures across the
        // call, so bridge through a reference box.
        final class Box { var value: FakeKeychain; init(_ v: FakeKeychain) { value = v } }
        let box = Box(keychain)
        RemoteConnectionStore.migrateLegacyStorageIfNeeded(
            defaults: defaults,
            loadScopedToken: {
                box.value.tokens[$0].map(KeychainReadResult.found) ?? .notFound
            },
            loadScopedRelayCredentials: {
                box.value.relay[$0].map(RemoteKeychain.RelayCredentialState.available)
                    ?? .missing
            },
            loadLegacyToken: {
                box.value.legacyToken.map(KeychainReadResult.found) ?? .notFound
            },
            loadLegacyRelayCredentials: {
                box.value.legacyRelay.map(RemoteKeychain.RelayCredentialState.available)
                    ?? .missing
            },
            saveToken: { token, macID in
                guard tokenWriteSucceeds else { return false }
                box.value.tokens[macID] = token
                return true
            },
            saveRelayCredentials: { creds, macID in
                guard relayWriteSucceeds else { return false }
                box.value.relay[macID] = creds
                return true
            },
            deleteLegacyKeychainItems: {
                box.value.legacyToken = nil
                box.value.legacyRelay = nil
            }
        )
        keychain = box.value
    }

    private func seedLegacy(_ rec: PairedMacRecord) {
        defaults.set(try! JSONEncoder().encode(rec), forKey: legacyRecordKey)
    }

    private var sampleRelay: RelayCredentials {
        RelayCredentials(
            relayURL: URL(string: "wss://relay.unpeel.com")!,
            macID: "mac-1",
            relayToken: "relay-token",
            e2eKey: Data(repeating: 7, count: 32)
        )
    }

    private func pairingResponse(
        authToken: String = "new-bearer",
        endpoint: URL = URL(string: "http://192.168.1.10:4485")!,
        directEndpoint: URL? = nil
    ) -> RemotePairingResponse {
        RemotePairingResponse(
            macID: "mac-1",
            macName: "Studio",
            endpoint: endpoint,
            directEndpoint: directEndpoint,
            deviceID: "device-1",
            authToken: authToken,
            pairedAtUnixMs: 2,
            relayCredentials: sampleRelay
        )
    }

    func testUnavailableLegacyMigrationMakesNoWritesOrDeletes() throws {
        enum UnavailableSlot: CaseIterable {
            case scopedToken
            case scopedRelay
            case legacyToken
            case legacyRelay
        }

        for unavailableSlot in UnavailableSlot.allCases {
            defaults.removePersistentDomain(forName: suiteName)
            let legacy = record(macID: "mac-1", name: "Build 12 Mac")
            let existing = [record(macID: "mac-existing", name: "Existing")]
            seedLegacy(legacy)
            defaults.set(try JSONEncoder().encode(existing), forKey: recordsKey)
            defaults.set("mac-existing", forKey: activeMacIDKey)
            defaults.set("stable-phone-id", forKey: deviceIDKey)
            let legacyBlob = try XCTUnwrap(defaults.data(forKey: legacyRecordKey))
            let recordsBlob = try XCTUnwrap(defaults.data(forKey: recordsKey))
            var tokenWrites = 0
            var relayWrites = 0
            var deletes = 0

            let result = RemoteConnectionStore.migrateLegacyStorageIfNeeded(
                defaults: defaults,
                loadScopedToken: { _ in
                    unavailableSlot == .scopedToken
                        ? .temporarilyUnavailable(errSecInteractionNotAllowed)
                        : .notFound
                },
                loadScopedRelayCredentials: { _ in
                    unavailableSlot == .scopedRelay
                        ? .temporarilyUnavailable(errSecNotAvailable)
                        : .missing
                },
                loadLegacyToken: {
                    unavailableSlot == .legacyToken
                        ? .temporarilyUnavailable(errSecInteractionNotAllowed)
                        : .found("legacy-bearer")
                },
                loadLegacyRelayCredentials: {
                    unavailableSlot == .legacyRelay
                        ? .temporarilyUnavailable(errSecNotAvailable)
                        : .available(self.sampleRelay)
                },
                saveToken: { _, _ in
                    tokenWrites += 1
                    return true
                },
                saveRelayCredentials: { _, _ in
                    relayWrites += 1
                    return true
                },
                deleteLegacyKeychainItems: { deletes += 1 }
            )

            let expectedStatus: OSStatus
            switch unavailableSlot {
            case .scopedToken, .legacyToken:
                expectedStatus = errSecInteractionNotAllowed
            case .scopedRelay, .legacyRelay:
                expectedStatus = errSecNotAvailable
            }
            XCTAssertEqual(result, .temporarilyUnavailable(expectedStatus))
            XCTAssertTrue(result.needsRetry)
            XCTAssertEqual(tokenWrites, 0)
            XCTAssertEqual(relayWrites, 0)
            XCTAssertEqual(deletes, 0)
            XCTAssertEqual(defaults.data(forKey: legacyRecordKey), legacyBlob)
            XCTAssertEqual(defaults.data(forKey: recordsKey), recordsBlob)
            XCTAssertEqual(defaults.string(forKey: activeMacIDKey), "mac-existing")
            XCTAssertEqual(defaults.string(forKey: deviceIDKey), "stable-phone-id")
            XCTAssertFalse(
                RemotePairingPresentationPolicy.needsPairing(
                    isPaired: false,
                    keychainHydrationPending: result.needsRetry,
                    devBridgeAvailable: false
                ),
                "legacy-only protected data must not present QR pairing"
            )
        }
    }

    func testDeferredLegacyMigrationCompletesAfterProtectedDataReturns() {
        let legacy = record(macID: "mac-1", name: "Build 12 Mac")
        seedLegacy(legacy)
        defaults.set("stable-phone-id", forKey: deviceIDKey)
        var writes = 0
        var deletes = 0

        let deferred = RemoteConnectionStore.migrateLegacyStorageIfNeeded(
            defaults: defaults,
            loadScopedToken: { _ in .notFound },
            loadScopedRelayCredentials: { _ in .missing },
            loadLegacyToken: { .temporarilyUnavailable(errSecInteractionNotAllowed) },
            loadLegacyRelayCredentials: { .available(self.sampleRelay) },
            saveToken: { _, _ in
                writes += 1
                return true
            },
            saveRelayCredentials: { _, _ in
                writes += 1
                return true
            },
            deleteLegacyKeychainItems: { deletes += 1 }
        )

        XCTAssertEqual(deferred, .temporarilyUnavailable(errSecInteractionNotAllowed))
        XCTAssertEqual(writes, 0)
        XCTAssertEqual(deletes, 0)
        XCTAssertTrue(storedRecords().isEmpty)
        XCTAssertNotNil(defaults.data(forKey: legacyRecordKey))

        var keychain = FakeKeychain(
            legacyToken: "legacy-bearer",
            legacyRelay: sampleRelay
        )
        runMigration(&keychain)

        XCTAssertEqual(storedRecords(), [legacy])
        XCTAssertEqual(defaults.string(forKey: activeMacIDKey), legacy.macID)
        XCTAssertEqual(defaults.string(forKey: deviceIDKey), "stable-phone-id")
        XCTAssertEqual(keychain.tokens[legacy.macID], "legacy-bearer")
        XCTAssertEqual(keychain.relay[legacy.macID], sampleRelay)
        XCTAssertNil(defaults.data(forKey: legacyRecordKey))
        XCTAssertNil(keychain.legacyToken)
        XCTAssertNil(keychain.legacyRelay)
    }

    func testMigrationMovesLegacyRecordAndCredentials() {
        seedLegacy(record(macID: "mac-1", name: "Legacy Mac"))
        var keychain = FakeKeychain(legacyToken: "bearer-1", legacyRelay: sampleRelay)

        runMigration(&keychain)

        XCTAssertEqual(storedRecords().map(\.macID), ["mac-1"])
        XCTAssertEqual(defaults.string(forKey: activeMacIDKey), "mac-1")
        XCTAssertEqual(keychain.tokens["mac-1"], "bearer-1")
        XCTAssertEqual(keychain.relay["mac-1"], sampleRelay)
        XCTAssertNil(defaults.data(forKey: legacyRecordKey), "legacy record must be deleted")
        XCTAssertNil(keychain.legacyToken, "legacy keychain items must be deleted")
        XCTAssertNil(keychain.legacyRelay)
    }

    func testMigrationRunTwiceIsIdentical() {
        seedLegacy(record(macID: "mac-1"))
        var keychain = FakeKeychain(legacyToken: "bearer-1", legacyRelay: sampleRelay)
        runMigration(&keychain)
        let firstRecords = storedRecords()
        let firstActive = defaults.string(forKey: activeMacIDKey)
        let firstTokens = keychain.tokens

        runMigration(&keychain) // legacy key gone → no-op fast path

        XCTAssertEqual(storedRecords(), firstRecords)
        XCTAssertEqual(defaults.string(forKey: activeMacIDKey), firstActive)
        XCTAssertEqual(keychain.tokens, firstTokens)
    }

    func testMigrationTokenWriteFailurePreservesLegacyPairingForRetry() {
        let legacy = record(macID: "mac-1", name: "Build 12 Mac")
        seedLegacy(legacy)
        defaults.set("stable-phone-id", forKey: deviceIDKey)
        var keychain = FakeKeychain(legacyToken: "bearer-1", legacyRelay: sampleRelay)

        runMigration(&keychain, tokenWriteSucceeds: false)

        XCTAssertEqual(
            try? JSONDecoder().decode(
                PairedMacRecord.self,
                from: XCTUnwrap(defaults.data(forKey: legacyRecordKey))
            ),
            legacy
        )
        XCTAssertTrue(storedRecords().isEmpty)
        XCTAssertEqual(defaults.string(forKey: deviceIDKey), "stable-phone-id")
        XCTAssertEqual(keychain.legacyToken, "bearer-1")
        XCTAssertEqual(keychain.legacyRelay, sampleRelay)
    }

    func testMigrationNeverOverwritesFreshScopedTokenAfterFailedLegacyCopy() {
        let legacy = record(macID: "mac-1", name: "Build 12 Mac")
        seedLegacy(legacy)
        var keychain = FakeKeychain(legacyToken: "old-bearer")

        runMigration(&keychain, tokenWriteSucceeds: false)
        keychain.tokens[legacy.macID] = "fresh-bearer"
        runMigration(&keychain, tokenWriteSucceeds: false)

        XCTAssertEqual(keychain.tokens[legacy.macID], "fresh-bearer")
        XCTAssertEqual(storedRecords(), [legacy])
        XCTAssertNil(defaults.data(forKey: legacyRecordKey))
        XCTAssertNil(keychain.legacyToken)
    }

    func testMigrationRelayWriteFailureRetainsValidLegacyCredentialForRetry() {
        let legacy = record(macID: "mac-1", name: "Build 12 Mac")
        seedLegacy(legacy)
        defaults.set("stable-phone-id", forKey: deviceIDKey)
        var keychain = FakeKeychain(legacyToken: "bearer-1", legacyRelay: sampleRelay)

        runMigration(&keychain, relayWriteSucceeds: false)

        XCTAssertEqual(storedRecords(), [legacy])
        XCTAssertEqual(defaults.string(forKey: activeMacIDKey), legacy.macID)
        XCTAssertEqual(defaults.string(forKey: deviceIDKey), "stable-phone-id")
        XCTAssertEqual(keychain.tokens[legacy.macID], "bearer-1")
        XCTAssertNotNil(defaults.data(forKey: legacyRecordKey))
        XCTAssertEqual(keychain.legacyToken, "bearer-1")
        XCTAssertEqual(keychain.legacyRelay, sampleRelay)
        XCTAssertNil(keychain.relay[legacy.macID])
        XCTAssertTrue(
            RelayCredentialRefreshMarker.needsRefresh(
                macID: legacy.macID,
                state: .missing,
                defaults: defaults
            )
        )

        // A later launch can finish the same migration without duplicating
        // the already-scoped record or changing either stable identity.
        runMigration(&keychain)

        XCTAssertEqual(storedRecords(), [legacy])
        XCTAssertEqual(defaults.string(forKey: activeMacIDKey), legacy.macID)
        XCTAssertEqual(defaults.string(forKey: deviceIDKey), "stable-phone-id")
        XCTAssertEqual(keychain.relay[legacy.macID], sampleRelay)
        XCTAssertNil(defaults.data(forKey: legacyRecordKey))
        XCTAssertNil(keychain.legacyToken)
        XCTAssertNil(keychain.legacyRelay)
    }

    func testMigrationNeverOverwritesDirectRecoveredScopedRelayCredential() {
        let legacy = record(macID: "mac-1", name: "Build 12 Mac")
        seedLegacy(legacy)
        var keychain = FakeKeychain(legacyToken: "bearer-1", legacyRelay: sampleRelay)

        runMigration(&keychain, relayWriteSucceeds: false)
        let recoveredRelay = RelayCredentials(
            relayURL: sampleRelay.relayURL,
            macID: sampleRelay.macID,
            relayToken: "relay-token-2",
            e2eKey: Data(repeating: 9, count: 32)
        )
        keychain.relay[legacy.macID] = recoveredRelay
        runMigration(&keychain, relayWriteSucceeds: false)

        XCTAssertEqual(keychain.relay[legacy.macID], recoveredRelay)
        XCTAssertEqual(storedRecords(), [legacy])
        XCTAssertNil(defaults.data(forKey: legacyRecordKey))
        XCTAssertNil(keychain.legacyToken)
        XCTAssertNil(keychain.legacyRelay)
        XCTAssertTrue(
            RelayCredentialRefreshMarker.isCurrent(
                macID: legacy.macID,
                defaults: defaults
            )
        )
    }

    func testMigrationDropsStructurallyInvalidLegacyRelayCredential() {
        let legacy = record(macID: "mac-1", name: "Build 12 Mac")
        seedLegacy(legacy)
        let invalidRelay = RelayCredentials(
            relayURL: URL(string: "wss://relay.unpeel.com")!,
            macID: "wrong-mac",
            relayToken: "relay-token",
            e2eKey: Data(repeating: 7, count: 32)
        )
        var keychain = FakeKeychain(
            legacyToken: "bearer-1",
            legacyRelay: invalidRelay
        )

        runMigration(&keychain)

        XCTAssertEqual(storedRecords(), [legacy])
        XCTAssertEqual(keychain.tokens[legacy.macID], "bearer-1")
        XCTAssertTrue(keychain.relay.isEmpty)
        XCTAssertNil(defaults.data(forKey: legacyRecordKey))
        XCTAssertNil(keychain.legacyToken)
        XCTAssertNil(keychain.legacyRelay)
    }

    func testPartialRunDoesNotDuplicateOrClobberActive() {
        // Simulate a crash after the new keys were written but before the
        // legacy ones were deleted — plus the user having since paired and
        // switched to another Mac.
        seedLegacy(record(macID: "mac-1"))
        let existing = [record(macID: "mac-1"), record(macID: "mac-2")]
        defaults.set(try! JSONEncoder().encode(existing), forKey: recordsKey)
        defaults.set("mac-2", forKey: activeMacIDKey)
        var keychain = FakeKeychain(
            tokens: ["mac-1": "bearer-1", "mac-2": "bearer-2"],
            legacyToken: "bearer-1"
        )

        runMigration(&keychain)

        XCTAssertEqual(storedRecords().map(\.macID), ["mac-1", "mac-2"], "no duplicate mac-1")
        XCTAssertEqual(defaults.string(forKey: activeMacIDKey), "mac-2", "active Mac untouched")
        XCTAssertEqual(keychain.tokens["mac-2"], "bearer-2")
        XCTAssertNil(defaults.data(forKey: legacyRecordKey))
        XCTAssertNil(keychain.legacyToken)
    }

    func testMigrationWithCorruptLegacyBlobStillCleansUp() {
        defaults.set(Data("not json".utf8), forKey: legacyRecordKey)
        var keychain = FakeKeychain(legacyToken: "orphan")

        runMigration(&keychain)

        XCTAssertTrue(storedRecords().isEmpty)
        XCTAssertNil(defaults.string(forKey: activeMacIDKey))
        XCTAssertNil(defaults.data(forKey: legacyRecordKey), "corrupt blob still removed")
        XCTAssertNil(keychain.legacyToken, "orphaned legacy keychain items still removed")
    }

    func testMigrationNoLegacyDataIsNoOp() {
        var keychain = FakeKeychain(tokens: ["mac-9": "keep-me"])
        defaults.set("mac-9", forKey: activeMacIDKey)

        runMigration(&keychain)

        XCTAssertTrue(storedRecords().isEmpty)
        XCTAssertEqual(defaults.string(forKey: activeMacIDKey), "mac-9")
        XCTAssertEqual(keychain.tokens["mac-9"], "keep-me")
    }

    // MARK: - Pairing commit

    func testPairingTokenWriteFailureDoesNotActivateRecordOrClaimSuccess() {
        let existing = [record(macID: "mac-1", name: "Existing Mac")]
        var relayWrites = 0

        XCTAssertThrowsError(
            try RemotePairingCommit.prepare(
                response: pairingResponse(),
                existingRecords: existing,
                saveToken: { _, _ in false },
                saveRelayCredentials: { _, _ in
                    relayWrites += 1
                    return true
                }
            )
        ) { error in
            let pairingError = error as? PairingError
            XCTAssertTrue(pairingError?.message.contains("already replaced") == true)
            XCTAssertTrue(pairingError?.message.contains("fresh pairing code") == true)
        }

        XCTAssertEqual(relayWrites, 0, "Relay must not persist after bearer failure")
        XCTAssertEqual(existing[0].macName, "Existing Mac", "caller state stays untouched")
    }

    func testPairingRejectsEmptyBearerBeforeAnyPersistence() {
        var tokenWrites = 0
        var relayWrites = 0

        XCTAssertThrowsError(
            try RemotePairingCommit.prepare(
                response: pairingResponse(authToken: ""),
                existingRecords: [],
                saveToken: { _, _ in
                    tokenWrites += 1
                    return true
                },
                saveRelayCredentials: { _, _ in
                    relayWrites += 1
                    return true
                }
            )
        )

        XCTAssertEqual(tokenWrites, 0)
        XCTAssertEqual(relayWrites, 0)
    }

    func testPairingCommitUpsertsOnlyAfterTokenAndAllowsDirectWhenRelayWriteFails() throws {
        var writeOrder: [String] = []

        let commit = try RemotePairingCommit.prepare(
            response: pairingResponse(),
            existingRecords: [record(macID: "mac-1", name: "Existing Mac")],
            saveToken: { token, macID in
                writeOrder.append("token:\(macID):\(token)")
                return true
            },
            saveRelayCredentials: { _, macID in
                writeOrder.append("relay:\(macID)")
                return false
            }
        )

        XCTAssertEqual(writeOrder, ["token:mac-1:new-bearer", "relay:mac-1"])
        XCTAssertEqual(commit.records.count, 1)
        XCTAssertEqual(commit.records[0].macName, "Studio")
        XCTAssertEqual(commit.record.deviceID, "device-1")
        XCTAssertFalse(commit.relayCredentialsSaved)
    }

    func testControllerAssistedPairingPersistsTheHostsEndpointNotTheProxy() throws {
        let proxy = URL(
            string: "http://192.168.1.20:49152/mobile/pairing-proxy/INVITE-1"
        )!
        let direct = URL(string: "http://10.0.0.8:17661/mobile")!
        let commit = try RemotePairingCommit.prepare(
            response: pairingResponse(endpoint: proxy, directEndpoint: direct),
            existingRecords: [],
            saveToken: { _, _ in true },
            saveRelayCredentials: { _, _ in true }
        )

        XCTAssertEqual(commit.record.endpoint, direct)
        XCTAssertNotEqual(commit.record.endpoint, proxy)
    }

    // MARK: - Relay credential recovery

    func testBuild12CredentialMarkerMigrationKeepsPairingAndDeviceIDs() throws {
        let existing = record(macID: "mac-1", name: "Build 12 Mac")
        defaults.set(try JSONEncoder().encode([existing]), forKey: recordsKey)
        defaults.set(existing.macID, forKey: activeMacIDKey)
        defaults.set("stable-phone-id", forKey: deviceIDKey)
        let recordsBefore = defaults.data(forKey: recordsKey)
        let state = RemoteKeychain.RelayCredentialState.available(sampleRelay)

        XCTAssertTrue(
            RelayCredentialRefreshMarker.needsRefresh(
                macID: existing.macID,
                state: state,
                defaults: defaults
            ),
            "build 12 has no freshness marker and must rotate once over Direct"
        )

        RelayCredentialRefreshMarker.markCurrent(macID: existing.macID, defaults: defaults)

        XCTAssertFalse(
            RelayCredentialRefreshMarker.needsRefresh(
                macID: existing.macID,
                state: state,
                defaults: defaults
            )
        )
        XCTAssertEqual(defaults.data(forKey: recordsKey), recordsBefore)
        XCTAssertEqual(defaults.string(forKey: activeMacIDKey), existing.macID)
        XCTAssertEqual(defaults.string(forKey: deviceIDKey), "stable-phone-id")
    }

    func testMissingUndecodableAndStructurallyInvalidRelayCredentialsNeedRepair() {
        XCTAssertEqual(
            RemoteKeychain.relayCredentialState(data: nil, expectedMacID: "mac-1"),
            .missing
        )
        XCTAssertEqual(
            RemoteKeychain.relayCredentialState(
                data: Data("not json".utf8),
                expectedMacID: "mac-1"
            ),
            .invalid
        )
        let wrongLength = RelayCredentials(
            relayURL: URL(string: "wss://relay.unpeel.com")!,
            macID: "mac-1",
            relayToken: "relay-token",
            e2eKey: Data(repeating: 7, count: 16)
        )
        XCTAssertEqual(
            RemoteKeychain.relayCredentialState(
                data: try! JSONEncoder().encode(wrongLength),
                expectedMacID: "mac-1"
            ),
            .invalid
        )
        XCTAssertTrue(
            RelayCredentialRefreshMarker.needsRefresh(
                macID: "mac-1",
                state: .missing,
                defaults: defaults
            )
        )
        XCTAssertTrue(
            RelayCredentialRefreshMarker.needsRefresh(
                macID: "mac-1",
                state: .invalid,
                defaults: defaults
            )
        )
    }

    func testRelayFailureMarksOtherwiseValidCredentialsStale() {
        let state = RemoteKeychain.RelayCredentialState.available(sampleRelay)
        RelayCredentialRefreshMarker.markCurrent(macID: "mac-1", defaults: defaults)
        XCTAssertFalse(
            RelayCredentialRefreshMarker.needsRefresh(
                macID: "mac-1",
                state: state,
                defaults: defaults
            )
        )

        RelayCredentialRefreshMarker.markStale(macID: "mac-1", defaults: defaults)

        XCTAssertTrue(
            RelayCredentialRefreshMarker.needsRefresh(
                macID: "mac-1",
                state: state,
                defaults: defaults
            )
        )
    }

    func testRelayCredentialPersistenceReturnsFailureInsteadOfArmingState() {
        var writes = 0

        let saved = RemoteKeychain.persistRelayCredentials(
            sampleRelay,
            expectedMacID: "mac-1"
        ) { _ in
            writes += 1
            return false
        }

        XCTAssertFalse(saved)
        XCTAssertEqual(writes, 1)
    }

    func testRelayCredentialRepairReportsWriteFailure() async {
        var persisted: RelayCredentials?

        let outcome = await RelayCredentialRepair.attempt(
            expectedMacID: "mac-1",
            fetch: { self.sampleRelay },
            persist: {
                persisted = $0
                return false
            }
        )

        XCTAssertEqual(outcome, .persistenceFailed)
        XCTAssertEqual(persisted, sampleRelay)
    }

    func testRecoveryUnavailableKeepsValidBuild12CredentialWithoutRefreshLoop() async {
        let state = RemoteKeychain.RelayCredentialState.available(sampleRelay)

        let outcome = await RelayCredentialRepair.attempt(
            expectedMacID: "mac-1",
            fetch: { throw RemoteMacClientError(statusCode: 404, serverMessage: "not found") },
            persist: { _ in
                XCTFail("404 must not attempt a Keychain write")
                return false
            }
        )
        RelayCredentialRefreshMarker.markRecoveryUnavailable(
            macID: "mac-1",
            defaults: defaults
        )

        XCTAssertEqual(outcome, .recoveryUnavailable)
        XCTAssertFalse(
            RelayCredentialRefreshMarker.needsRefresh(
                macID: "mac-1",
                state: state,
                defaults: defaults
            )
        )
    }

    func testBoundRelayCredentialRepairReports404WithoutPersisting() async {
        let activeRecord = record(macID: "mac-1")
        let activeClient = RemoteMacClient(
            baseURL: activeRecord.endpoint,
            authToken: "bearer-1"
        )
        var persists = 0

        let result = await RelayCredentialRepair.attemptIfBoundToActiveDirectClient(
            candidate: activeClient,
            activeClient: activeClient,
            activeRecord: activeRecord,
            activeToken: "bearer-1",
            connectionEpoch: 3,
            isStillCurrent: { $0.epoch == 3 },
            fetch: {
                throw RemoteMacClientError(statusCode: 404, serverMessage: "not found")
            },
            persist: { _ in
                persists += 1
                return true
            }
        )

        XCTAssertEqual(result?.outcome, .recoveryUnavailable)
        XCTAssertEqual(result?.generation.epoch, 3)
        XCTAssertEqual(persists, 0)
    }

    func testBoundRelayCredentialRepairReportsPersistenceFailure() async {
        let activeRecord = record(macID: "mac-1")
        let activeClient = RemoteMacClient(
            baseURL: activeRecord.endpoint,
            authToken: "bearer-1"
        )

        let result = await RelayCredentialRepair.attemptIfBoundToActiveDirectClient(
            candidate: activeClient,
            activeClient: activeClient,
            activeRecord: activeRecord,
            activeToken: "bearer-1",
            connectionEpoch: 4,
            isStillCurrent: { $0.epoch == 4 },
            fetch: { self.sampleRelay },
            persist: { _ in false }
        )

        XCTAssertEqual(result?.outcome, .persistenceFailed)
        XCTAssertEqual(result?.generation.macID, "mac-1")
    }

    func testBoundRelayCredentialRepairSuccessCarriesExactGeneration() async {
        let activeRecord = record(macID: "mac-1")
        let activeClient = RemoteMacClient(
            baseURL: activeRecord.endpoint,
            authToken: "bearer-1"
        )
        var persisted: RelayCredentials?

        let result = await RelayCredentialRepair.attemptIfBoundToActiveDirectClient(
            candidate: activeClient,
            activeClient: activeClient,
            activeRecord: activeRecord,
            activeToken: "bearer-1",
            connectionEpoch: 5,
            isStillCurrent: {
                $0.matches(
                    epoch: 5,
                    activeClient: activeClient,
                    activeRecord: activeRecord,
                    activeToken: "bearer-1"
                )
            },
            fetch: { self.sampleRelay },
            persist: {
                persisted = $0
                return true
            }
        )

        XCTAssertEqual(result?.outcome, .refreshed)
        XCTAssertEqual(result?.generation.epoch, 5)
        XCTAssertEqual(result?.generation.authToken, "bearer-1")
        XCTAssertEqual(persisted, sampleRelay)
    }

    func testCrossMacDirectClientMismatchNeverFetchesRotatingCredentials() async {
        let activeRecord = record(
            macID: "mac-b",
            name: "Mac B",
            endpoint: "http://10.0.0.2:4485"
        )
        let activeClient = RemoteMacClient(
            baseURL: activeRecord.endpoint,
            authToken: "bearer-b"
        )
        let staleMacAClient = RemoteMacClient(
            baseURL: URL(string: "http://10.0.0.1:4485")!,
            authToken: "bearer-a"
        )
        var fetches = 0
        var persists = 0

        let outcome = await RelayCredentialRepair.attemptIfBoundToActiveDirectClient(
            candidate: staleMacAClient,
            activeClient: activeClient,
            activeRecord: activeRecord,
            activeToken: "bearer-b",
            connectionEpoch: 9,
            isStillCurrent: { _ in true },
            fetch: {
                fetches += 1
                return self.sampleRelay
            },
            persist: { _ in
                persists += 1
                return true
            }
        )

        XCTAssertNil(outcome)
        XCTAssertEqual(fetches, 0)
        XCTAssertEqual(persists, 0)
    }

    func testSameMacRePairDuringFetchDiscardsSupersededGeneration() async {
        let activeRecord = record(macID: "mac-1", name: "Mac A")
        var currentEpoch = 7
        var currentToken = "old-bearer"
        var currentClient = RemoteMacClient(
            baseURL: activeRecord.endpoint,
            authToken: currentToken
        )
        let candidate = currentClient
        var fetches = 0
        var persists = 0
        var supersededGeneration: RemoteDirectClientGeneration?
        RelayCredentialRefreshMarker.markCurrent(
            macID: activeRecord.macID,
            defaults: defaults
        )

        let result = await RelayCredentialRepair.attemptIfBoundToActiveDirectClient(
            candidate: candidate,
            activeClient: currentClient,
            activeRecord: activeRecord,
            activeToken: currentToken,
            connectionEpoch: currentEpoch,
            isStillCurrent: { generation in
                generation.matches(
                    epoch: currentEpoch,
                    activeClient: currentClient,
                    activeRecord: activeRecord,
                    activeToken: currentToken
                )
            },
            onSupersededAfterFetch: {
                supersededGeneration = $0
                RelayCredentialRefreshMarker.markStale(
                    macID: $0.macID,
                    defaults: self.defaults
                )
            },
            fetch: {
                fetches += 1
                // Model completePairing committing a new bearer for the same
                // macID while the old recovery request is suspended.
                currentEpoch = 8
                currentToken = "new-bearer"
                currentClient = RemoteMacClient(
                    baseURL: activeRecord.endpoint,
                    authToken: currentToken
                )
                return self.sampleRelay
            },
            persist: { _ in
                persists += 1
                return true
            }
        )

        XCTAssertNil(result)
        XCTAssertEqual(fetches, 1)
        XCTAssertEqual(persists, 0, "old generation must not overwrite fresh Relay credentials")
        XCTAssertEqual(supersededGeneration?.epoch, 7)
        XCTAssertTrue(
            RelayCredentialRefreshMarker.needsRefresh(
                macID: activeRecord.macID,
                state: .available(sampleRelay),
                defaults: defaults
            ),
            "an ambiguous Host rotation must be repaired on the next healthy poll"
        )
    }

    func testRelayFallbackCooldownStartsWhenSlowFailureCompletes() {
        let attemptStarted = Date(timeIntervalSince1970: 1_000)
        let failureCompleted = attemptStarted.addingTimeInterval(30)
        let retryAfter = RelayFallbackRetryPolicy.retryAfterFailure(
            completedAt: failureCompleted
        )

        XCTAssertEqual(
            retryAfter,
            failureCompleted.addingTimeInterval(RelayFallbackRetryPolicy.failureDelay)
        )
        XCTAssertFalse(
            RelayFallbackRetryPolicy.canAttempt(
                now: retryAfter.addingTimeInterval(-0.001),
                retryAfter: retryAfter
            ),
            "a failure slower than the cooldown still gets the full cooldown"
        )
        XCTAssertTrue(
            RelayFallbackRetryPolicy.canAttempt(now: retryAfter, retryAfter: retryAfter)
        )
    }

    func testSameMacRePairABADiscardsOldRelayToDirectProbe() async throws {
        let activeRecord = record(macID: "mac-1", name: "Mac A")
        var currentEpoch = 6
        var currentToken = "old-bearer"
        var currentRelayClient = RemoteMacClient(
            baseURL: activeRecord.endpoint,
            authToken: currentToken,
            relay: RemoteRelayConnection(
                credentials: sampleRelay,
                deviceID: activeRecord.deviceID
            )
        )
        let generation = try XCTUnwrap(
            RemoteRelayClientGeneration.capture(
                activeClient: currentRelayClient,
                activeRecord: activeRecord,
                activeToken: currentToken,
                epoch: currentEpoch
            )
        )
        var adoptions = 0

        let restored = await RemoteDirectRestore.attempt(
            generation: generation,
            isStillCurrent: {
                $0.matches(
                    epoch: currentEpoch,
                    activeClient: currentRelayClient,
                    activeRecord: activeRecord,
                    activeToken: currentToken
                )
            },
            probe: {
                // While the old Direct probe is suspended, the same Mac is
                // re-paired and its new generation later enters Relay again.
                currentEpoch = 7
                currentToken = "new-bearer"
                currentRelayClient = RemoteMacClient(
                    baseURL: activeRecord.endpoint,
                    authToken: currentToken,
                    relay: RemoteRelayConnection(
                        credentials: self.sampleRelay,
                        deviceID: activeRecord.deviceID
                    )
                )
                return RemoteBootstrapSnapshot(
                    macID: "mac-1",
                    macName: "Mac A",
                    folders: [],
                    projects: [],
                    presets: [],
                    sessions: [],
                    capturedAtUnixMs: 1
                )
            },
            adopt: { adoptions += 1 }
        )

        XCTAssertFalse(restored)
        XCTAssertEqual(adoptions, 0, "old bearer/client must not replace the new Relay generation")
        XCTAssertEqual(currentEpoch, 7)
        XCTAssertEqual(currentRelayClient.authToken, "new-bearer")
    }

    func testDirectRestoreRejectsMissingAndMismatchedHostIdentity() async throws {
        let active = record(macID: "mac-1", name: "Mac A")
        let relayClient = RemoteMacClient(
            baseURL: active.endpoint,
            authToken: "bearer-1",
            relay: RemoteRelayConnection(
                credentials: sampleRelay,
                deviceID: active.deviceID
            )
        )
        let generation = try XCTUnwrap(RemoteRelayClientGeneration.capture(
            activeClient: relayClient,
            activeRecord: active,
            activeToken: "bearer-1",
            epoch: 4
        ))
        var adoptions = 0

        let rejectedHostIDs: [String?] = [nil, "mac-other"]
        for hostID in rejectedHostIDs {
            let restored = await RemoteDirectRestore.attempt(
                generation: generation,
                isStillCurrent: { _ in true },
                probe: {
                    RemoteBootstrapSnapshot(
                        macID: hostID,
                        macName: "Unexpected",
                        folders: [],
                        projects: [],
                        presets: [],
                        sessions: [],
                        capturedAtUnixMs: 1
                    )
                },
                adopt: { adoptions += 1 }
            )
            XCTAssertFalse(restored)
        }
        XCTAssertEqual(adoptions, 0)
    }

    // MARK: - Relay-authenticated Direct endpoint refresh

    func testRelayBootstrapEndpointRefreshPersistsUpdatedRecordInPlace() throws {
        let other = record(
            macID: "mac-other",
            endpoint: "http://192.168.1.30:4400/mobile"
        )
        let active = record(
            macID: "mac-1",
            name: "Studio Mac",
            endpoint: "http://192.168.1.10:4485/mobile"
        )
        let relayClient = RemoteMacClient(
            baseURL: active.endpoint,
            authToken: "bearer-1",
            relay: RemoteRelayConnection(
                credentials: sampleRelay,
                deviceID: active.deviceID
            )
        )
        let refreshed = try XCTUnwrap(URL(
            string: "http://192.168.1.10:61234/mobile"
        ))
        let poll = RemoteConnectionPollProof(
            client: relayClient,
            connectionEpoch: 4,
            hostMacID: active.macID,
            directEndpoint: refreshed
        )

        let plan = try XCTUnwrap(RelayDirectEndpointRefresh.prepare(
            poll: poll,
            activeClient: relayClient,
            activeRecord: active,
            activeToken: "bearer-1",
            records: [other, active],
            epoch: 4
        ))

        XCTAssertEqual(plan.record.endpoint, refreshed)
        XCTAssertEqual(plan.records.map(\.macID), ["mac-other", "mac-1"])
        XCTAssertEqual(plan.records.last?.endpoint, refreshed)

        RemoteConnectionStore.saveRecords(plan.records, defaults: defaults)
        XCTAssertEqual(storedRecords(), plan.records)
        XCTAssertEqual(storedRecords().last?.deviceID, active.deviceID)
        XCTAssertEqual(storedRecords().last?.pairedAtUnixMs, active.pairedAtUnixMs)
    }

    func testRelayBootstrapEndpointRefreshRejectsMissingAndMismatchedHostIdentity() {
        let active = record(
            macID: "mac-1",
            endpoint: "http://192.168.1.10:4485/mobile"
        )
        let relayClient = RemoteMacClient(
            baseURL: active.endpoint,
            authToken: "bearer-1",
            relay: RemoteRelayConnection(
                credentials: sampleRelay,
                deviceID: active.deviceID
            )
        )
        for hostMacID: String? in [nil, "mac-attacker"] {
            let poll = RemoteConnectionPollProof(
                client: relayClient,
                connectionEpoch: 2,
                hostMacID: hostMacID,
                directEndpoint: URL(string: "http://10.0.0.99:61234/mobile")
            )

            XCTAssertNil(RelayDirectEndpointRefresh.prepare(
                poll: poll,
                activeClient: relayClient,
                activeRecord: active,
                activeToken: "bearer-1",
                records: [active],
                epoch: 2
            ))
        }
    }

    func testRelayBootstrapEndpointRefreshRejectsMalformedAndNonHTTPEndpoints() {
        let active = record(
            macID: "mac-1",
            endpoint: "http://192.168.1.10:4485/mobile"
        )
        let relayClient = RemoteMacClient(
            baseURL: active.endpoint,
            authToken: "bearer-1",
            relay: RemoteRelayConnection(
                credentials: sampleRelay,
                deviceID: active.deviceID
            )
        )
        // A TLS-capable Host may advertise `https://`; the stored endpoint is
        // the canonical `http://` spelling and the certificate pin decides
        // the wire scheme, so this is accepted — never sent to unpinned TLS.
        let httpsPoll = RemoteConnectionPollProof(
            client: relayClient,
            connectionEpoch: 3,
            hostMacID: active.macID,
            directEndpoint: URL(string: "https://192.168.1.10:61234/mobile")
        )
        XCTAssertEqual(
            RelayDirectEndpointRefresh.prepare(
                poll: httpsPoll,
                activeClient: relayClient,
                activeRecord: active,
                activeToken: "bearer-1",
                records: [active],
                epoch: 3
            )?.record.endpoint,
            URL(string: "http://192.168.1.10:61234/mobile")
        )

        let invalidEndpoints: [URL?] = [
            URL(string: "wss://192.168.1.10:61234/mobile"),
            URL(string: "ftp://192.168.1.10:61234/mobile"),
            URL(string: "http://192.168.1.10:61234/not-mobile"),
            URL(string: "http://user@192.168.1.10:61234/mobile"),
            URL(string: "http://192.168.1.10/mobile"),
            URL(string: "http://127.0.0.1:61234/mobile"),
            URL(string: "http://localhost:61234/mobile"),
            URL(string: "http://169.254.10.2:61234/mobile"),
            URL(string: "relative/mobile"),
            nil,
        ]

        for endpoint in invalidEndpoints {
            let poll = RemoteConnectionPollProof(
                client: relayClient,
                connectionEpoch: 3,
                hostMacID: active.macID,
                directEndpoint: endpoint
            )
            XCTAssertNil(
                RelayDirectEndpointRefresh.prepare(
                    poll: poll,
                    activeClient: relayClient,
                    activeRecord: active,
                    activeToken: "bearer-1",
                    records: [active],
                    epoch: 3
                ),
                "must reject \(endpoint?.absoluteString ?? "nil")"
            )
        }
    }

    func testRelayBootstrapEndpointRefreshDiscardsSupersededRelayGeneration() {
        let active = record(
            macID: "mac-1",
            endpoint: "http://192.168.1.10:4485/mobile"
        )
        let oldRelayClient = RemoteMacClient(
            baseURL: active.endpoint,
            authToken: "bearer-1",
            relay: RemoteRelayConnection(
                credentials: sampleRelay,
                deviceID: active.deviceID
            )
        )
        // Same endpoint/token/epoch but a different Relay actor models the ABA
        // case after the selected Host was re-paired and returned to Relay.
        let currentRelayClient = RemoteMacClient(
            baseURL: active.endpoint,
            authToken: "bearer-1",
            relay: RemoteRelayConnection(
                credentials: sampleRelay,
                deviceID: active.deviceID
            )
        )
        let poll = RemoteConnectionPollProof(
            client: oldRelayClient,
            connectionEpoch: 8,
            hostMacID: active.macID,
            directEndpoint: URL(string: "http://192.168.1.10:61234/mobile")
        )

        XCTAssertNil(RelayDirectEndpointRefresh.prepare(
            poll: poll,
            activeClient: currentRelayClient,
            activeRecord: active,
            activeToken: "bearer-1",
            records: [active],
            epoch: 8
        ))
        XCTAssertNil(RelayDirectEndpointRefresh.prepare(
            poll: poll,
            activeClient: oldRelayClient,
            activeRecord: active,
            activeToken: "bearer-1",
            records: [active],
            epoch: 9
        ))
    }
}
