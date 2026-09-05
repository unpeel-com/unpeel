import XCTest
import CUnpeelNativeBridge
import UnpeelShared
@testable import UnpeelNative

final class NativeRemoteBackendTests: XCTestCase {
    func testOpenRejectsUnsafeSSHTargetWithStructuredError() {
        XCTAssertThrowsError(try NativeRemoteBackend(sshTarget: "ssh://studio:2222")) { error in
            guard let error = error as? NativeRemoteBackendError else {
                return XCTFail("unexpected error: \(error)")
            }
            XCTAssertEqual(error.code, "invalid_ssh_target")
            XCTAssertTrue(error.message.contains("SSH config alias"))
        }
    }

    func testOpenIsLazyAndCloseIsIdempotent() async throws {
        let backend = try NativeRemoteBackend(sshTarget: "ssh://studio")
        XCTAssertFalse(backend.isClosed)

        await backend.close()
        XCTAssertTrue(backend.isClosed)
        await backend.close()
        XCTAssertTrue(backend.isClosed)
    }

    func testDirectOpenIsLazyAndRejectsNonPairedScope() async throws {
        let endpoint = try XCTUnwrap(URL(string: "http://127.0.0.1:9/mobile"))
        let backend = try NativeRemoteBackend(
            directEndpoint: endpoint,
            authToken: "paired-device-token",
            expectedHostID: "host"
        )
        XCTAssertFalse(backend.isClosed)
        await backend.close()
        XCTAssertTrue(backend.isClosed)

        let unsafe = try XCTUnwrap(URL(string: "https://example.test/mobile"))
        XCTAssertThrowsError(try NativeRemoteBackend(
            directEndpoint: unsafe,
            authToken: "paired-device-token",
            expectedHostID: "host"
        )) { error in
            XCTAssertEqual(
                (error as? NativeRemoteBackendError)?.code,
                "invalid_host_endpoint"
            )
        }
    }

    func testLinkOpenRequiresCredentialsForTheDurablyPinnedHost() throws {
        let credentials = RelayCredentials(
            relayURL: try XCTUnwrap(URL(string: "wss://link.example.test")),
            macID: "different-host",
            relayToken: "relay-token",
            e2eKey: Data(repeating: 1, count: 32)
        )
        XCTAssertThrowsError(try NativeRemoteBackend(
            relayCredentials: credentials,
            controllerDeviceID: "controller",
            authToken: "host-bearer",
            expectedHostID: "saved-host"
        )) { error in
            XCTAssertEqual(
                (error as? NativeRemoteBackendError)?.code,
                "invalid_link_host_identity"
            )
        }
    }

    func testEffectOutcomeUnknownIsNeverConfusedWithOrdinaryFailure() {
        let uncertain = NativeRemoteBackendError(
            result: -5,
            code: "host_disconnected",
            message: "receipt lost",
            kind: "outcomeUnknown",
            operation: "terminal write"
        )
        let rejected = NativeRemoteBackendError(
            result: -5,
            code: "host_operation_rejected",
            message: "not accepted",
            kind: "notApplied",
            operation: "terminal write"
        )

        XCTAssertTrue(uncertain.effectOutcomeIsUnknown)
        XCTAssertFalse(rejected.effectOutcomeIsUnknown)
        XCTAssertFalse(uncertain.effectWasNotApplied)
        XCTAssertTrue(rejected.effectWasNotApplied)
        XCTAssertFalse(uncertain.effectCanContinueOnCurrentGeneration)
        XCTAssertTrue(rejected.effectCanContinueOnCurrentGeneration)

        let transportNotSent = NativeRemoteBackendError(
            result: -5,
            code: "host_connection_disconnected",
            message: "no bytes sent",
            kind: "notApplied",
            operation: "terminal write"
        )
        XCTAssertTrue(transportNotSent.effectWasNotApplied)
        XCTAssertFalse(transportNotSent.effectCanContinueOnCurrentGeneration)
    }

    func testOutputDefaultsRespectCoreLimitAndPageLeaseResolvesOnce() {
        XCTAssertEqual(NativeRemoteBackend.maximumOutputPageBytes, 200 * 1024)
        let page = NativeRemoteOutputPage(
            metadata: NativeRemoteOutputPageMetadata(
                sessionID: "session",
                requestedOffset: nil,
                offset: 0,
                nextOffset: 3,
                resetBeforeFeed: false,
                truncated: false,
                capturedAtUnixMs: 1,
                byteCount: 3
            ),
            bytes: Data("abc".utf8),
            parentHandle: 41,
            pageHandle: 42
        )

        XCTAssertEqual(page.claimResolution()?.parent, 41)
        XCTAssertNil(page.claimResolution())
    }

    func testMalformedEffectReceiptsConservativelySuspendRetries() {
        XCTAssertThrowsError(try NativeRemoteBackend.decodeEffect(
            result: Int32(UNPEEL_NATIVE_BRIDGE_OK),
            pointer: nil,
            length: 0,
            operation: "terminal write"
        )) { error in
            let error = error as? NativeRemoteBackendError
            XCTAssertEqual(error?.code, "invalid_remote_effect_receipt")
            XCTAssertTrue(error?.effectOutcomeIsUnknown == true)
        }

        let malformedFailure = NativeRemoteBackend.effectBridgeError(
            result: Int32(UNPEEL_NATIVE_BRIDGE_ERROR_REMOTE),
            output: Data("not-json".utf8),
            operation: "terminal write"
        )
        XCTAssertEqual(malformedFailure.code, "invalid_remote_effect_failure")
        XCTAssertTrue(malformedFailure.effectOutcomeIsUnknown)
    }

    func testClosedBackendRefusesBootstrapWithoutLaunchingSSH() async throws {
        let backend = try NativeRemoteBackend(sshTarget: "ssh://studio")
        await backend.close()

        do {
            _ = try await backend.bootstrap()
            XCTFail("expected closed backend failure")
        } catch let error as NativeRemoteBackendError {
            XCTAssertEqual(error.code, "remote_backend_closed")
        }
    }

    func testOutputAndEffectsRequireIdentityValidatedBootstrap() async throws {
        let backend = try NativeRemoteBackend(
            directEndpoint: try XCTUnwrap(URL(string: "http://127.0.0.1:9/mobile")),
            authToken: "paired-device-token",
            expectedHostID: "saved-host"
        )
        defer { Task { await backend.close() } }

        do {
            _ = try await backend.pollOutput(
                sessionID: "session",
                limit: 1,
                waitMilliseconds: 0
            )
            XCTFail("expected pre-bootstrap output refusal")
        } catch let error as NativeRemoteBackendError {
            XCTAssertEqual(error.code, "remote_backend_not_bootstrapped")
        }

        do {
            _ = try await backend.writeTerminal(
                sessionID: "session",
                data: Data("x".utf8)
            )
            XCTFail("expected pre-bootstrap effect refusal")
        } catch let error as NativeRemoteBackendError {
            XCTAssertEqual(error.code, "remote_backend_not_bootstrapped")
        }
    }

    func testSavedHostIdentityMustMatchBootstrapBeforePublish() throws {
        let snapshot = RemoteBootstrapSnapshot(
            macID: "actual-host",
            macName: "Studio",
            folders: [],
            projects: [],
            presets: [],
            sessions: [],
            capturedAtUnixMs: 1
        )

        XCTAssertNoThrow(try NativeRemoteBackend.validateHostIdentity(
            snapshot,
            expectedHostID: "actual-host"
        ))
        XCTAssertThrowsError(try NativeRemoteBackend.validateHostIdentity(
            snapshot,
            expectedHostID: "saved-host"
        )) { error in
            XCTAssertEqual((error as? NativeRemoteBackendError)?.code, "host_identity_changed")
        }

        let anonymous = RemoteBootstrapSnapshot(
            macID: nil,
            macName: "Anonymous",
            folders: [],
            projects: [],
            presets: [],
            sessions: [],
            capturedAtUnixMs: 1
        )
        XCTAssertThrowsError(try NativeRemoteBackend.validateHostIdentity(
            anonymous,
            expectedHostID: "saved-host"
        )) { error in
            XCTAssertEqual((error as? NativeRemoteBackendError)?.code, "host_identity_changed")
        }
    }
}
