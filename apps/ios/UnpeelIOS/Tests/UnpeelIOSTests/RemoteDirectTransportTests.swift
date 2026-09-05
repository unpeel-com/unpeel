import XCTest
import UnpeelShared
@testable import UnpeelIOS

/// Direct `/mobile` transport selection (pinned HTTPS vs legacy plaintext),
/// the URLs each client builds, the plaintext refusal path, the relay
/// bootstrap deadline, and push-token routing. All socket-free.
final class RemoteDirectTransportTests: XCTestCase {
    private let fingerprint = "ab" + String(repeating: "cd", count: 31)
    private let endpoint = URL(string: "http://192.168.1.10:4485/mobile")!

    private func record(
        directTLSFingerprint: String? = nil,
        remoteServerCertificateFingerprint: String? = nil
    ) -> PairedMacRecord {
        PairedMacRecord(
            macID: "mac-1",
            macName: "Studio",
            endpoint: endpoint,
            deviceID: "device-1",
            pairedAtUnixMs: 1,
            remoteServerCertificateFingerprint: remoteServerCertificateFingerprint,
            directTLSFingerprint: directTLSFingerprint
        )
    }

    // MARK: - URL construction

    func testPinnedClientRewritesStoredHTTPEndpointToHTTPSOnSamePort() throws {
        let url = RemoteMacClient.directRequestURL(
            baseURL: endpoint,
            path: "bootstrap",
            query: [:],
            pinnedCertificateFingerprint: fingerprint
        )
        XCTAssertEqual(url.absoluteString, "https://192.168.1.10:4485/mobile/bootstrap")
    }

    func testUnpinnedClientKeepsPlaintextEndpointForOlderHosts() {
        let url = RemoteMacClient.directRequestURL(
            baseURL: endpoint,
            path: "bootstrap",
            query: [:],
            pinnedCertificateFingerprint: nil
        )
        XCTAssertEqual(url.absoluteString, "http://192.168.1.10:4485/mobile/bootstrap")
    }

    func testPinnedClientPreservesPathAndSortsQueryItems() {
        let url = RemoteMacClient.directRequestURL(
            baseURL: endpoint,
            path: "output",
            query: ["session_id": "s-1", "limit": "10", "offset": "5"],
            pinnedCertificateFingerprint: fingerprint
        )
        XCTAssertEqual(
            url.absoluteString,
            "https://192.168.1.10:4485/mobile/output?limit=10&offset=5&session_id=s-1"
        )
    }

    func testDirectClientFromRecordCarriesThePinAndKeepsStoredBaseURL() {
        let pinned = record(directTLSFingerprint: fingerprint.uppercased())
            .directClient(token: "bearer")
        XCTAssertEqual(pinned.baseURL, endpoint)
        XCTAssertEqual(pinned.pinnedCertificateFingerprint, fingerprint)
        XCTAssertFalse(pinned.sendsBearerInPlaintext)

        let plaintext = record().directClient(token: "bearer")
        XCTAssertNil(plaintext.pinnedCertificateFingerprint)
        XCTAssertTrue(plaintext.sendsBearerInPlaintext)

        let relay = pinned.viaRelay(RemoteRelayConnection(
            credentials: RelayCredentials(
                relayURL: URL(string: "wss://relay.example.test")!,
                macID: "mac-1",
                relayToken: "relay-token",
                e2eKey: Data(repeating: 0x42, count: 32)
            ),
            deviceID: "device-1"
        ))
        XCTAssertTrue(relay.isRelay)
        XCTAssertFalse(relay.sendsBearerInPlaintext)
        XCTAssertEqual(relay.pinnedCertificateFingerprint, fingerprint)
    }

    func testHTTPSAdvertisedEndpointsAreStoredCanonicallyAsHTTP() throws {
        let advertised = try XCTUnwrap(URL(string: "https://192.168.1.10:4485/mobile"))
        XCTAssertEqual(
            RemoteDirectTransportPolicy.canonicalStoredEndpoint(advertised),
            endpoint
        )
        XCTAssertEqual(
            RelayDirectEndpointRefresh.validatedHTTPMobileEndpoint(advertised),
            endpoint
        )
    }

    // MARK: - Transport decision

    func testCapabilityFlagSelectsTLS() {
        let decision = RemoteDirectTransportPolicy.decision(
            for: RemoteDirectTransportAdvertisement(
                certificateFingerprint: fingerprint.uppercased(),
                serverVersion: nil,
                hostCapabilities: ["host.bootstrap", RemoteControlProtocol.mobileTLSCapability]
            )
        )
        XCTAssertEqual(decision, .tls(fingerprint: fingerprint))
    }

    func testServerVersionAtOrAfterMinimumSelectsTLSWithoutCapability() {
        for version in ["0.5.3", "0.5.10", "0.6.0", "1.0.0", "v0.5.3", "0.5.3-beta.1"] {
            let decision = RemoteDirectTransportPolicy.decision(
                for: RemoteDirectTransportAdvertisement(
                    certificateFingerprint: fingerprint,
                    serverVersion: version,
                    hostCapabilities: ["host.bootstrap"]
                )
            )
            XCTAssertEqual(decision, .tls(fingerprint: fingerprint), version)
        }
    }

    func testOlderServerVersionSelectsPlaintext() {
        for version in ["0.5.2", "0.4.9", "0.5"] {
            let decision = RemoteDirectTransportPolicy.decision(
                for: RemoteDirectTransportAdvertisement(
                    certificateFingerprint: fingerprint,
                    serverVersion: version,
                    hostCapabilities: nil
                )
            )
            XCTAssertEqual(decision, .plaintext, version)
        }
    }

    func testNoSignalKeepsCurrentTransport() {
        let decision = RemoteDirectTransportPolicy.decision(
            for: RemoteDirectTransportAdvertisement(
                certificateFingerprint: fingerprint,
                serverVersion: nil,
                hostCapabilities: ["host.bootstrap"]
            )
        )
        XCTAssertEqual(decision, .unknown)
        XCTAssertEqual(
            RemoteDirectTransportPolicy.decision(
                for: RemoteDirectTransportAdvertisement(
                    certificateFingerprint: nil,
                    serverVersion: "garbage",
                    hostCapabilities: nil
                )
            ),
            .unknown
        )
    }

    func testTLSSignalWithoutFingerprintIsUnpinnableAndChangesNothing() {
        let decision = RemoteDirectTransportPolicy.decision(
            for: RemoteDirectTransportAdvertisement(
                certificateFingerprint: "  ",
                serverVersion: "0.5.3",
                hostCapabilities: nil
            )
        )
        XCTAssertEqual(decision, .tlsUnpinnable)
        XCTAssertNil(RemoteDirectTransportPolicy.applying(
            decision, to: record(), authenticated: true
        ))
        XCTAssertNil(RemoteDirectTransportPolicy.applying(
            decision, to: record(directTLSFingerprint: fingerprint), authenticated: true
        ))
    }

    func testUpgradeToTLSIsAcceptedFromPlaintextBootstrap() throws {
        let updated = try XCTUnwrap(RemoteDirectTransportPolicy.applying(
            .tls(fingerprint: fingerprint),
            to: record(),
            authenticated: false
        ))
        XCTAssertEqual(updated.directTLSFingerprint, fingerprint)
        XCTAssertNil(RemoteDirectTransportPolicy.applying(
            .tls(fingerprint: fingerprint),
            to: updated,
            authenticated: false
        ), "re-applying the same pin is a no-op")
    }

    func testDowngradeToPlaintextRequiresAnAuthenticatedSource() {
        let pinned = record(directTLSFingerprint: fingerprint)
        XCTAssertNil(
            RemoteDirectTransportPolicy.applying(.plaintext, to: pinned, authenticated: false),
            "a plaintext LAN reply must never strip the pin"
        )
        let cleared = RemoteDirectTransportPolicy.applying(
            .plaintext, to: pinned, authenticated: true
        )
        XCTAssertNil(cleared?.directTLSFingerprint)
        XCTAssertNil(
            RemoteDirectTransportPolicy.applying(.plaintext, to: record(), authenticated: true),
            "already plaintext"
        )
    }

    func testPairingCommitPinsATLSEraHostBeforeTheFirstBearerRequest() throws {
        let response = RemotePairingResponse(
            macID: "mac-1",
            macName: "Studio",
            endpoint: endpoint,
            directEndpoint: URL(string: "https://192.168.1.10:4485/mobile"),
            deviceID: "device-1",
            authToken: "bearer",
            pairedAtUnixMs: 1,
            remoteServerCertificateFingerprint: fingerprint.uppercased(),
            relayCredentials: RelayCredentials(
                relayURL: URL(string: "wss://relay.example.test")!,
                macID: "mac-1",
                relayToken: "relay-token",
                e2eKey: Data(repeating: 0x42, count: 32)
            ),
            serverVersion: "0.5.3"
        )
        let commit = try RemotePairingCommit.prepare(
            response: response,
            existingRecords: [],
            saveToken: { _, _ in true },
            saveRelayCredentials: { _, _ in true }
        )
        XCTAssertEqual(commit.record.endpoint, endpoint)
        XCTAssertEqual(commit.record.directTLSFingerprint, fingerprint)
        XCTAssertEqual(commit.record.remoteServerCertificateFingerprint, fingerprint)
        XCTAssertFalse(commit.record.directClient(token: "bearer").sendsBearerInPlaintext)
    }

    func testPairingCommitLeavesOlderHostsOnPlaintext() throws {
        let response = RemotePairingResponse(
            macID: "mac-1",
            macName: "Studio",
            endpoint: endpoint,
            deviceID: "device-1",
            authToken: "bearer",
            pairedAtUnixMs: 1,
            remoteServerCertificateFingerprint: fingerprint,
            relayCredentials: RelayCredentials(
                relayURL: URL(string: "wss://relay.example.test")!,
                macID: "mac-1",
                relayToken: "relay-token",
                e2eKey: Data(repeating: 0x42, count: 32)
            )
        )
        let commit = try RemotePairingCommit.prepare(
            response: response,
            existingRecords: [],
            saveToken: { _, _ in true },
            saveRelayCredentials: { _, _ in true }
        )
        XCTAssertNil(commit.record.directTLSFingerprint)
        XCTAssertEqual(commit.record.remoteServerCertificateFingerprint, fingerprint)
    }

    func testRecordsStoredBeforeTheTransportFieldsDecodeAsPlaintext() throws {
        let legacy = #"{"macID":"mac-1","macName":"Studio","endpoint":"http://192.168.1.10:4485/mobile","deviceID":"device-1","pairedAtUnixMs":1}"#
        let decoded = try JSONDecoder().decode(PairedMacRecord.self, from: Data(legacy.utf8))
        XCTAssertNil(decoded.directTLSFingerprint)
        XCTAssertNil(decoded.remoteServerCertificateFingerprint)
        XCTAssertTrue(decoded.directClient(token: "bearer").sendsBearerInPlaintext)

        let pinned = record(directTLSFingerprint: fingerprint)
        let roundTripped = try JSONDecoder().decode(
            PairedMacRecord.self,
            from: JSONEncoder().encode(pinned)
        )
        XCTAssertEqual(roundTripped, pinned)
    }

    // MARK: - Plaintext refusal

    func testPlaintextRefusalIsRecognizedFrom426AndFromA401ThatNamesHTTPS() {
        XCTAssertTrue(RemoteMacClientError(statusCode: 426, serverMessage: nil).requiresTLS)
        XCTAssertTrue(RemoteMacClientError(
            statusCode: 426, serverMessage: "Upgrade Required"
        ).requiresTLS)
        XCTAssertTrue(RemoteMacClientError(
            statusCode: 401, serverMessage: "use https"
        ).requiresTLS)
        XCTAssertTrue(RemoteMacClientError(
            statusCode: 401, serverMessage: "bearer tokens are only accepted over TLS"
        ).requiresTLS)
        XCTAssertFalse(RemoteMacClientError(
            statusCode: 401, serverMessage: "invalid token"
        ).requiresTLS)
        XCTAssertFalse(RemoteMacClientError(statusCode: 401, serverMessage: nil).requiresTLS)
        XCTAssertFalse(RemoteMacClientError(statusCode: 403, serverMessage: "https").requiresTLS)
        XCTAssertFalse(RemoteMacClientError(statusCode: 500, serverMessage: nil).requiresTLS)
    }

    func testDirectGenerationTreatsThePlaintextAndPinnedClientsAsDifferent() throws {
        let pinnedRecord = record(directTLSFingerprint: fingerprint)
        let plaintext = record().directClient(token: "bearer")
        let pinned = pinnedRecord.directClient(token: "bearer")

        XCTAssertNil(RemoteDirectClientGeneration.capture(
            candidate: plaintext,
            activeClient: plaintext,
            activeRecord: pinnedRecord,
            activeToken: "bearer",
            epoch: 3
        ), "a plaintext client is never a generation of a pinned record")

        let generation = try XCTUnwrap(RemoteDirectClientGeneration.capture(
            candidate: plaintext,
            activeClient: plaintext,
            activeRecord: record(),
            activeToken: "bearer",
            epoch: 3
        ))
        XCTAssertFalse(generation.matches(
            epoch: 3,
            activeClient: pinned,
            activeRecord: pinnedRecord,
            activeToken: "bearer"
        ), "after the refusal upgrade the old plaintext generation is superseded")
        XCTAssertTrue(generation.matches(
            epoch: 3,
            activeClient: plaintext,
            activeRecord: record(),
            activeToken: "bearer"
        ))
    }

    @MainActor
    func testPollFailureWithPlaintextRefusalUpgradesInsteadOfPaintingAnOutage() async {
        let refusal = RemoteMacClientError(statusCode: 426, serverMessage: "use https")
        let store = RemotePreviewStore(
            snapshot: .empty,
            client: record().directClient(token: "bearer"),
            createSessionOverride: nil,
            bootstrapOverride: { throw refusal },
            defaults: UserDefaults(suiteName: "RemoteDirectTransportTests.\(UUID())")!
        )
        store.adoptClient(record().directClient(token: "bearer"), connectionEpoch: 7)
        var upgradedEpochs: [Int] = []
        store.onDirectPlaintextRefused = { epoch in
            upgradedEpochs.append(epoch)
            // The connection owner republishes a pinned generation.
            store.adoptClient(
                self.record(directTLSFingerprint: self.fingerprint).directClient(token: "bearer"),
                connectionEpoch: 8
            )
            return true
        }

        let result = await store.loadFromBridge()

        XCTAssertEqual(upgradedEpochs, [7])
        guard case .superseded = result else {
            return XCTFail("expected the refused plaintext poll to be superseded, got \(result)")
        }
        XCTAssertFalse(store.isDisconnected, "a transport upgrade is not an outage")
        XCTAssertFalse(store.client.sendsBearerInPlaintext)
    }

    @MainActor
    func testPollFailureWithPlaintextRefusalThatCannotUpgradeIsAnOrdinaryOutage() async {
        let refusal = RemoteMacClientError(statusCode: 426, serverMessage: "use https")
        let store = RemotePreviewStore(
            snapshot: .empty,
            client: record().directClient(token: "bearer"),
            createSessionOverride: nil,
            bootstrapOverride: { throw refusal },
            defaults: UserDefaults(suiteName: "RemoteDirectTransportTests.\(UUID())")!
        )
        store.adoptClient(record().directClient(token: "bearer"), connectionEpoch: 7)
        store.onDirectPlaintextRefused = { _ in false }

        let result = await store.loadFromBridge()

        guard case .currentFailure = result else {
            return XCTFail("expected an ordinary failure, got \(result)")
        }
        XCTAssertTrue(store.isDisconnected)
    }

    @MainActor
    func testPinnedClientNeverConsultsTheRefusalHook() async {
        let refusal = RemoteMacClientError(statusCode: 426, serverMessage: "use https")
        let store = RemotePreviewStore(
            snapshot: .empty,
            client: record(directTLSFingerprint: fingerprint).directClient(token: "bearer"),
            createSessionOverride: nil,
            bootstrapOverride: { throw refusal },
            defaults: UserDefaults(suiteName: "RemoteDirectTransportTests.\(UUID())")!
        )
        var consulted = false
        store.onDirectPlaintextRefused = { _ in
            consulted = true
            return true
        }

        _ = await store.loadFromBridge()

        XCTAssertFalse(consulted)
    }

    // MARK: - Bootstrap staging

    @MainActor
    func testBootstrapReadinessFollowsTheCurrentClientGeneration() async {
        let store = RemotePreviewStore(
            snapshot: .empty,
            client: record().directClient(token: "bearer"),
            createSessionOverride: nil,
            bootstrapOverride: {
                RemoteBootstrapSnapshot(
                    macID: "mac-1",
                    folders: [],
                    projects: [],
                    presets: [],
                    sessions: [],
                    capturedAtUnixMs: 1
                )
            },
            defaults: UserDefaults(suiteName: "RemoteDirectTransportTests.\(UUID())")!
        )
        XCTAssertFalse(store.hasBootstrapForCurrentClient)

        _ = await store.loadFromBridge()
        XCTAssertTrue(store.hasBootstrapForCurrentClient)

        // A new generation (relay fallback, re-pair, TLS upgrade) must stage
        // the terminal stream behind its own first bootstrap again.
        store.adoptClient(
            record(directTLSFingerprint: fingerprint).directClient(token: "bearer"),
            connectionEpoch: 2
        )
        XCTAssertFalse(store.hasBootstrapForCurrentClient)

        _ = await store.loadFromBridge()
        XCTAssertTrue(store.hasBootstrapForCurrentClient)

        // Re-adopting the same generation is not a reset.
        store.adoptClient(store.client, connectionEpoch: 2)
        XCTAssertTrue(store.hasBootstrapForCurrentClient)
    }

    @MainActor
    func testSuccessfulPollCarriesTheHostTransportAdvertisement() async throws {
        let store = RemotePreviewStore(
            snapshot: .empty,
            client: record().directClient(token: "bearer"),
            createSessionOverride: nil,
            bootstrapOverride: {
                RemoteBootstrapSnapshot(
                    hostProtocol: RemoteHostProtocolDescriptor(
                        capabilities: [RemoteControlProtocol.mobileTLSCapability]
                    ),
                    macID: "mac-1",
                    folders: [],
                    projects: [],
                    presets: [],
                    sessions: [],
                    capturedAtUnixMs: 1,
                    remoteServerCertificateFingerprint: self.fingerprint,
                    serverVersion: "0.5.3"
                )
            },
            defaults: UserDefaults(suiteName: "RemoteDirectTransportTests.\(UUID())")!
        )

        let result = await store.loadFromBridge()
        guard case .success(let proof) = result else {
            return XCTFail("expected success, got \(result)")
        }
        let advertisement = try XCTUnwrap(proof.directTransport)
        XCTAssertEqual(advertisement.certificateFingerprint, fingerprint)
        XCTAssertEqual(advertisement.serverVersion, "0.5.3")
        XCTAssertEqual(advertisement.hostCapabilities, [RemoteControlProtocol.mobileTLSCapability])
        XCTAssertFalse(proof.isTransportAuthenticated, "a plaintext LAN reply is not authenticated")
        XCTAssertEqual(
            RemoteDirectTransportPolicy.decision(for: advertisement),
            .tls(fingerprint: fingerprint)
        )
    }

    // MARK: - Bootstrap deadline

    func testBootstrapDeadlineIsFourSecondsOnTheLANAndTenOverTheRelay() {
        XCTAssertEqual(RemoteBootstrapDeadline.seconds(isRelay: false, measuredRoundTrip: nil), 4)
        XCTAssertEqual(RemoteBootstrapDeadline.seconds(isRelay: false, measuredRoundTrip: 3), 4)
        XCTAssertEqual(RemoteBootstrapDeadline.seconds(isRelay: true, measuredRoundTrip: nil), 10)
    }

    func testRelayBootstrapDeadlineScalesWithMeasuredRoundTripWithinBounds() {
        XCTAssertEqual(RemoteBootstrapDeadline.seconds(isRelay: true, measuredRoundTrip: 0.3), 10)
        XCTAssertEqual(RemoteBootstrapDeadline.seconds(isRelay: true, measuredRoundTrip: 2.5), 12.5)
        XCTAssertEqual(RemoteBootstrapDeadline.seconds(isRelay: true, measuredRoundTrip: 9), 20)
        XCTAssertEqual(RemoteBootstrapDeadline.seconds(isRelay: true, measuredRoundTrip: 0), 10)
        XCTAssertEqual(RemoteBootstrapDeadline.seconds(isRelay: true, measuredRoundTrip: .nan), 10)
    }

    // MARK: - Push token routing

    func testActiveMacOnRelayRegistersOverTheLiveConnectionOnly() {
        XCTAssertEqual(
            PushTokenRegistrationRoute.plan(
                isActiveMac: true, usingRelay: true, hasRelayCredentials: true
            ),
            [.activeRelayClient]
        )
    }

    func testDirectMacsTryTheLANThenALinkConnection() {
        XCTAssertEqual(
            PushTokenRegistrationRoute.plan(
                isActiveMac: true, usingRelay: false, hasRelayCredentials: true
            ),
            [.direct, .transientRelay]
        )
        XCTAssertEqual(
            PushTokenRegistrationRoute.plan(
                isActiveMac: false, usingRelay: true, hasRelayCredentials: true
            ),
            [.direct, .transientRelay],
            "the active Mac's relay state says nothing about another Mac"
        )
        XCTAssertEqual(
            PushTokenRegistrationRoute.plan(
                isActiveMac: false, usingRelay: false, hasRelayCredentials: false
            ),
            [.direct]
        )
    }

    // MARK: - Server version parsing

    func testServerVersionParsingAndOrdering() {
        XCTAssertEqual(RemoteServerVersion("0.5.3"), RemoteServerVersion(major: 0, minor: 5, patch: 3))
        XCTAssertEqual(RemoteServerVersion("0.10.0-beta.2+build.7"), RemoteServerVersion(major: 0, minor: 10, patch: 0))
        XCTAssertEqual(RemoteServerVersion("1"), RemoteServerVersion(major: 1, minor: 0, patch: 0))
        XCTAssertNil(RemoteServerVersion(nil))
        XCTAssertNil(RemoteServerVersion(""))
        XCTAssertNil(RemoteServerVersion("0.5.x"))
        XCTAssertNil(RemoteServerVersion("0.5.3.1"))
        XCTAssertTrue(RemoteServerVersion("0.5.10")! > RemoteServerVersion("0.5.3")!)
        XCTAssertTrue(RemoteServerVersion("0.6.0")! > RemoteServerVersion("0.5.99")!)
        XCTAssertTrue(RemoteServerVersion("0.5.2")! < RemoteServerVersion("0.5.3")!)
    }
}
