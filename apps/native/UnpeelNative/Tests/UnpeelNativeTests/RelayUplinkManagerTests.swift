import XCTest
import UnpeelShared
@testable import UnpeelNative

private final class MigrationTestE2EKeyStore: MobileE2EKeyStoring {
    var keys: [String: Data] = [:]
    func load(deviceID: String) -> Data? { keys[deviceID] }
    func save(_ key: Data, deviceID: String) throws { keys[deviceID] = key }
    func delete(deviceID: String) { keys[deviceID] = nil }
}

@MainActor
final class RelayUplinkManagerTests: XCTestCase {
    private func makeAuthorityHome() throws -> (url: URL, cleanup: () -> Void) {
        let url = FileManager.default.temporaryDirectory
            .appendingPathComponent("unpeel-link-authority-\(UUID().uuidString)")
        try FileManager.default.createDirectory(at: url, withIntermediateDirectories: true)
        return (url, { try? FileManager.default.removeItem(at: url) })
    }

    private func makeMigrationFixture(
        devices: String
    ) throws -> (store: MobilePairingStore, cleanup: () -> Void) {
        let dir = FileManager.default.temporaryDirectory
            .appendingPathComponent("unpeel-relay-push-\(UUID().uuidString)")
        try FileManager.default.createDirectory(at: dir, withIntermediateDirectories: true)
        let storageURL = dir.appendingPathComponent("devices.json")
        try Data(devices.utf8).write(to: storageURL)
        let store = MobilePairingStore(
            storageURL: storageURL,
            macID: "mac-1",
            macName: "Studio Mac",
            e2eKeyStore: MigrationTestE2EKeyStore()
        )
        return (store, { try? FileManager.default.removeItem(at: dir) })
    }

    /// Direct-only devices are not push targets either: pushes ride the Link
    /// service, so "Direct only" must mean no relay traffic at all.
    func testPushTargetsSkipDirectOnlyDevices() throws {
        let fixture = try makeMigrationFixture(devices: """
        {"version":1,"devices":[
          {"id":"phone-1","name":"iPhone","platform":"iOS","tokenHash":"a","pairedAtUnixMs":1,"relayTokenHash":"ha","apnsToken":"t1"},
          {"id":"phone-2","name":"iPad","platform":"iPadOS","tokenHash":"b","pairedAtUnixMs":2,"relayTokenHash":"hb","apnsToken":"t2","relayAllowed":false}
        ]}
        """)
        defer { fixture.cleanup() }

        XCTAssertEqual(fixture.store.pushTargets().map(\.deviceID), ["phone-1"])
    }

    func testPushFailureDiagnosticsDistinguishOperatorActions() {
        XCTAssertEqual(
            RelayUplinkManager.pushFailureLabel(reason: "no-entitlement"),
            "Link entitlement unavailable"
        )
        XCTAssertEqual(
            RelayUplinkManager.pushFailureLabel(reason: "BadDeviceToken"),
            "APNs rejected the device token"
        )
        XCTAssertEqual(
            RelayUplinkManager.pushFailureLabel(reason: "network"),
            "Could not reach Unpeel Link"
        )
    }

    func testRelayHandshakeTreatsBothUnauthorizedStatusesAsAuthorityEvents() {
        XCTAssertTrue(RelayUplinkManager.isAuthorizationRejection(401))
        XCTAssertTrue(RelayUplinkManager.isAuthorizationRejection(403))
        XCTAssertFalse(RelayUplinkManager.isAuthorizationRejection(402))
        XCTAssertFalse(RelayUplinkManager.isAuthorizationRejection(nil))
    }

    func testDurableSuppressionPrecedesAndSurvivesCacheRemovalFailure() throws {
        let fixture = try makeAuthorityHome()
        defer { fixture.cleanup() }
        let cache = fixture.url.appendingPathComponent("mobile/relay-entitlement.json")
        // A directory at the cache path makes POSIX unlink fail while staying
        // readable enough to reproduce the restart-safety edge.
        try FileManager.default.createDirectory(at: cache, withIntermediateDirectories: true)

        let outcome = try LinkAuthorityStore.suppress(
            home: fixture.url,
            reason: .userDisabled
        )

        XCTAssertNotNil(outcome.cacheRemovalError)
        let restarted = try LinkAuthorityStore.localState(home: fixture.url, macID: "host-1")
        XCTAssertEqual(restarted.suppression, outcome.record)
        XCTAssertNil(restarted.cached)
        XCTAssertEqual(restarted.suppression?.reason, .userDisabled)
        let marker = fixture.url.appendingPathComponent("link-disabled.json")
        let permissions = try FileManager.default.attributesOfItem(atPath: marker.path)[.posixPermissions]
            as? NSNumber
        XCTAssertEqual(permissions?.intValue, 0o600)
    }

    func testMalformedSuppressionFailsClosed() throws {
        let fixture = try makeAuthorityHome()
        defer { fixture.cleanup() }
        try Data("not-json".utf8).write(
            to: fixture.url.appendingPathComponent("link-disabled.json")
        )

        XCTAssertThrowsError(
            try LinkAuthorityStore.localState(home: fixture.url, macID: "host-1")
        )
    }

    func testLateEntitlementCannotClearNewerSuppressionGeneration() throws {
        let fixture = try makeAuthorityHome()
        defer { fixture.cleanup() }
        let original = try LinkAuthorityStore.suppress(
            home: fixture.url,
            reason: .authorizationRejected
        ).record
        let newer = try LinkAuthorityStore.suppress(
            home: fixture.url,
            reason: .userDisabled
        ).record
        let entitlement = LinkCachedEntitlement(
            entitlement: "late-bearer",
            expiresAt: Int64(Date().timeIntervalSince1970) + 3600,
            macID: "host-1"
        )

        XCTAssertThrowsError(
            try LinkAuthorityStore.commit(
                entitlement,
                expectedSuppressionGeneration: original.generation,
                home: fixture.url
            )
        )
        XCTAssertEqual(
            try LinkAuthorityStore.suppression(home: fixture.url),
            newer
        )
    }

    func testLateAuthorizationRejectionCannotWeakenUserDisable() throws {
        let fixture = try makeAuthorityHome()
        defer { fixture.cleanup() }
        let disabled = try LinkAuthorityStore.suppress(
            home: fixture.url,
            reason: .userDisabled
        ).record

        let rejected = try LinkAuthorityStore.suppress(
            home: fixture.url,
            reason: .authorizationRejected
        ).record

        XCTAssertEqual(rejected, disabled)
        XCTAssertEqual(
            try LinkAuthorityStore.suppression(home: fixture.url)?.reason,
            .userDisabled
        )
    }

    func testActivationPendingSurvivesRestartAndNeedsFreshEntitlement() throws {
        let fixture = try makeAuthorityHome()
        defer { fixture.cleanup() }
        let disabled = try LinkAuthorityStore.suppress(
            home: fixture.url,
            reason: .userDisabled
        ).record

        let generation = try LinkAuthorityStore.markActivationPending(
            home: fixture.url,
            expectedSuppressionGeneration: disabled.generation
        )
        let restarted = try LinkAuthorityStore.localState(home: fixture.url, macID: "host-1")

        XCTAssertEqual(generation, disabled.generation)
        XCTAssertEqual(restarted.suppression?.reason, .activationPending)
        XCTAssertNil(restarted.cached)

        let entitlement = LinkCachedEntitlement(
            entitlement: "fresh-after-restart",
            expiresAt: Int64(Date().timeIntervalSince1970) + 3600,
            macID: "host-1"
        )
        try LinkAuthorityStore.commit(
            entitlement,
            expectedSuppressionGeneration: generation,
            home: fixture.url
        )
        let recovered = try LinkAuthorityStore.localState(home: fixture.url, macID: "host-1")
        XCTAssertNil(recovered.suppression)
        XCTAssertEqual(recovered.cached, entitlement)
    }

    func testFreshActivationCannotReuseLegacyCachedBearer() throws {
        let fixture = try makeAuthorityHome()
        defer { fixture.cleanup() }
        let cache = fixture.url.appendingPathComponent("mobile/relay-entitlement.json")
        try FileManager.default.createDirectory(
            at: cache.deletingLastPathComponent(),
            withIntermediateDirectories: true
        )
        try JSONEncoder().encode(
            LinkCachedEntitlement(
                entitlement: "old-key-bearer",
                expiresAt: Int64(Date().timeIntervalSince1970) + 30 * 24 * 3600,
                macID: "host-1"
            )
        ).write(to: cache)

        let generation = try LinkAuthorityStore.markActivationPending(
            home: fixture.url,
            expectedSuppressionGeneration: nil
        )
        let restarted = try LinkAuthorityStore.localState(home: fixture.url, macID: "host-1")

        XCTAssertNotNil(generation)
        XCTAssertEqual(restarted.suppression?.reason, .activationPending)
        XCTAssertNil(restarted.cached)
        XCTAssertFalse(FileManager.default.fileExists(atPath: cache.path))
    }

    func testDeactivationDuringActivationWinsGenerationRace() throws {
        let fixture = try makeAuthorityHome()
        defer { fixture.cleanup() }
        let observed = try LinkAuthorityStore.suppress(
            home: fixture.url,
            reason: .authorizationRejected
        ).record
        let disabled = try LinkAuthorityStore.suppress(
            home: fixture.url,
            reason: .userDisabled
        ).record

        XCTAssertThrowsError(
            try LinkAuthorityStore.markActivationPending(
                home: fixture.url,
                expectedSuppressionGeneration: observed.generation
            )
        )
        XCTAssertEqual(try LinkAuthorityStore.suppression(home: fixture.url), disabled)
    }

    func testFreshEntitlementClearsOnlyCapturedSuppression() throws {
        let fixture = try makeAuthorityHome()
        defer { fixture.cleanup() }
        let suppression = try LinkAuthorityStore.suppress(
            home: fixture.url,
            reason: .authorizationRejected
        ).record
        let entitlement = LinkCachedEntitlement(
            entitlement: "fresh-bearer",
            expiresAt: Int64(Date().timeIntervalSince1970) + 3600,
            macID: "host-1"
        )

        try LinkAuthorityStore.commit(
            entitlement,
            expectedSuppressionGeneration: suppression.generation,
            home: fixture.url
        )

        let state = try LinkAuthorityStore.localState(home: fixture.url, macID: "host-1")
        XCTAssertNil(state.suppression)
        XCTAssertEqual(state.cached, entitlement)
    }
}
