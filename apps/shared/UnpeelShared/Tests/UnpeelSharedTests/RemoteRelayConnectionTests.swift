import Foundation
import XCTest
import UnpeelShared

final class RemoteRelayConnectionTests: XCTestCase {
    private func streamFinishesWithin(
        _ stream: AsyncStream<RelayStreamPush>,
        nanoseconds: UInt64
    ) async -> Bool {
        await withTaskGroup(of: Bool.self) { group in
            group.addTask {
                var iterator = stream.makeAsyncIterator()
                _ = await iterator.next()
                return true
            }
            group.addTask {
                try? await Task.sleep(nanoseconds: nanoseconds)
                return false
            }
            let first = await group.next() ?? false
            group.cancelAll()
            return first
        }
    }

    func testPublicSharedConnectionIsLazyAndReplacesPushConsumer() async {
        let credentials = RelayCredentials(
            relayURL: URL(string: "wss://relay.example.test")!,
            macID: "host-1",
            relayToken: "relay-token",
            e2eKey: Data(repeating: 0x42, count: 32)
        )
        let connection = RemoteRelayConnection(
            credentials: credentials,
            deviceID: "controller-1"
        )

        let first = await connection.outputPushFrames()
        var iterator = first.makeAsyncIterator()
        _ = await connection.outputPushFrames()

        // Registering the successor finishes the prior stream without ever
        // opening a socket. Both iOS and macOS can therefore construct this
        // shared downlink and defer network work until the first request.
        let firstAfterReplacement = await iterator.next()
        XCTAssertNil(firstAfterReplacement)
    }

    func testGenerationBoundRequestFailsClosedWithoutOpeningRelay() async throws {
        let credentials = RelayCredentials(
            relayURL: try XCTUnwrap(URL(string: "wss://relay.invalid.test")),
            macID: "host-1",
            relayToken: "relay-token-must-not-appear",
            e2eKey: Data(repeating: 0x42, count: 32)
        )
        let connection = RemoteRelayConnection(
            credentials: credentials,
            deviceID: "controller-1"
        )
        let request = RelayTunnelRequest(
            id: 17,
            method: "POST",
            path: "/mobile/write",
            auth: "Bearer host-token-must-not-appear",
            contentType: "application/json",
            body: Data(#"{"data":"x"}"#.utf8)
        )

        do {
            _ = try await connection.perform(
                request: request,
                requiredConnectionGeneration: 41,
                timeout: 1
            )
            XCTFail("a bound effect must never reconnect onto an unknown generation")
        } catch let error as RemoteRelayConnectionError {
            guard case .generationChanged = error else {
                return XCTFail("unexpected Link failure: \(error)")
            }
            XCTAssertFalse(error.localizedDescription.contains("relay-token-must-not-appear"))
            XCTAssertFalse(error.localizedDescription.contains("host-token-must-not-appear"))
        }
    }

    func testOversizedEnvelopeIsProvenNotSentAndRedactsSecrets() async throws {
        let relaySecret = "relay-token-must-not-appear"
        let bearerSecret = "host-token-must-not-appear"
        let credentials = RelayCredentials(
            relayURL: try XCTUnwrap(URL(string: "wss://relay.invalid.test")),
            macID: "host-1",
            relayToken: relaySecret,
            e2eKey: Data(repeating: 0x42, count: 32)
        )
        let connection = RemoteRelayConnection(
            credentials: credentials,
            deviceID: "controller-1"
        )
        // Base64 expansion guarantees the complete JSON envelope exceeds the
        // sealed-frame limit. Validation happens before connect or seal.
        let request = RelayTunnelRequest(
            id: 18,
            method: "POST",
            path: "/mobile/write",
            auth: "Bearer \(bearerSecret)",
            contentType: "application/octet-stream",
            body: Data(repeating: 0x61, count: RelayProtocol.maxPlaintextBytes)
        )

        do {
            _ = try await connection.perform(
                request: request,
                requiredConnectionGeneration: nil,
                timeout: 1
            )
            XCTFail("an oversized Link envelope must fail before transport")
        } catch let error as RemoteRelayConnectionError {
            guard case let .transport(delivery, message) = error else {
                return XCTFail("unexpected Link failure: \(error)")
            }
            XCTAssertEqual(delivery, .notSent)
            XCTAssertFalse(message.contains(relaySecret))
            XCTAssertFalse(message.contains(bearerSecret))
            XCTAssertFalse(error.localizedDescription.contains(relaySecret))
            XCTAssertFalse(error.localizedDescription.contains(bearerSecret))
        }
    }

    func testFreshConnectionAttemptDoesNotFinishRegisteredPushConsumer() async throws {
        let credentials = RelayCredentials(
            relayURL: try XCTUnwrap(URL(string: "wss://relay.invalid.test")),
            macID: "host-1",
            relayToken: "relay-token",
            // A malformed key fails establishment immediately and
            // deterministically, without touching the network.
            e2eKey: Data(repeating: 0x42, count: 31)
        )
        let connection = RemoteRelayConnection(
            credentials: credentials,
            deviceID: "controller-1"
        )
        let pushFrames = await connection.outputPushFrames()
        let request = RelayTunnelRequest(
            id: 19,
            method: "GET",
            path: "/relay/output-stream",
            auth: "Bearer paired-token"
        )

        do {
            _ = try await connection.perform(
                request: request,
                requiredConnectionGeneration: nil,
                timeout: 1
            )
            XCTFail("invalid E2E credentials must fail before transport")
        } catch let error as RemoteRelayConnectionError {
            guard case let .transport(delivery, _) = error else {
                return XCTFail("unexpected Link failure: \(error)")
            }
            XCTAssertEqual(delivery, .notSent)
        }

        // Connection setup is not a socket-loss teardown. The registered
        // push stream remains open so a successful retry can use it.
        let pushStreamFinished = await streamFinishesWithin(
            pushFrames,
            nanoseconds: 50_000_000
        )
        XCTAssertFalse(pushStreamFinished)
        await connection.close()
    }
}
