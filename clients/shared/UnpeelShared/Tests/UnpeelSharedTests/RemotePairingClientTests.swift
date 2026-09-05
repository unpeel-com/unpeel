import Foundation
import XCTest
@testable import UnpeelShared

final class RemotePairingClientTests: XCTestCase {
    func testPairPostsSealedRequestAndOpensBoundResponse() async throws {
        let fixture = try PairingFixture()
        let recorder = RequestRecorder()
        let responseBody = try sealedResponse(payload: fixture.payload, device: fixture.device)
        let client = RemotePairingClient { request in
            await recorder.record(request)
            return (
                responseBody,
                try XCTUnwrap(HTTPURLResponse(
                    url: request.url!,
                    statusCode: 200,
                    httpVersion: nil,
                    headerFields: nil
                ))
            )
        }

        let paired = try await client.pair(
            payload: fixture.payload,
            device: fixture.device,
            now: fixture.now
        )

        XCTAssertEqual(paired.macID, fixture.payload.macID)
        XCTAssertEqual(paired.endpoint, fixture.payload.endpoint)
        let recordedRequest = await recorder.request
        let request = try XCTUnwrap(recordedRequest)
        XCTAssertEqual(request.url?.absoluteString, "http://10.0.0.4:17661/mobile/pair")
        XCTAssertEqual(request.httpMethod, "POST")
        XCTAssertEqual(request.timeoutInterval, 10)
        XCTAssertEqual(request.value(forHTTPHeaderField: "Content-Type"), "application/json")

        let body = try XCTUnwrap(request.httpBody)
        let envelope = try JSONDecoder().decode(RemotePairingEnvelope.self, from: body)
        let plaintext = try RemotePairingCrypto.open(
            envelope,
            token: fixture.payload.token,
            macID: fixture.payload.macID,
            endpoint: fixture.payload.endpoint,
            direction: .request
        )
        XCTAssertEqual(
            try JSONDecoder().decode(RemotePairingRequest.self, from: plaintext),
            RemotePairingRequest(token: fixture.payload.token, device: fixture.device)
        )
    }

    func testExpiredPayloadFailsBeforeHTTP() async throws {
        let fixture = try PairingFixture()
        let recorder = RequestRecorder()
        let client = RemotePairingClient { request in
            await recorder.record(request)
            throw TestFailure.unexpectedRequest
        }

        do {
            _ = try await client.pair(
                payload: fixture.payload,
                device: fixture.device,
                now: Date(timeIntervalSince1970: 3)
            )
            XCTFail("expected expired pairing code to fail")
        } catch let error as RemotePairingClientError {
            XCTAssertEqual(error, .expired)
        }
        let recordedRequest = await recorder.request
        XCTAssertNil(recordedRequest)
    }

    func testRejectsResponseForDifferentHostIdentity() async throws {
        let fixture = try PairingFixture()
        let body = try sealedResponse(
            payload: fixture.payload,
            device: fixture.device,
            responseMacID: "different-host"
        )
        let client = successfulClient(body: body)

        do {
            _ = try await client.pair(
                payload: fixture.payload,
                device: fixture.device,
                now: fixture.now
            )
            XCTFail("expected Host identity mismatch")
        } catch let error as RemotePairingClientError {
            XCTAssertEqual(error, .responseHostIdentityMismatch)
        }
    }

    func testRejectsResponseForDifferentEndpoint() async throws {
        let fixture = try PairingFixture()
        let otherEndpoint = try XCTUnwrap(URL(string: "http://10.0.0.9:17661/mobile"))
        let body = try sealedResponse(
            payload: fixture.payload,
            device: fixture.device,
            responseEndpoint: otherEndpoint
        )
        let client = successfulClient(body: body)

        do {
            _ = try await client.pair(
                payload: fixture.payload,
                device: fixture.device,
                now: fixture.now
            )
            XCTFail("expected endpoint mismatch")
        } catch let error as RemotePairingClientError {
            XCTAssertEqual(error, .responseEndpointMismatch)
        }
    }

    func testRejectsIncompatibleResponseProtocol() async throws {
        let fixture = try PairingFixture()
        let body = try sealedResponse(
            payload: fixture.payload,
            device: fixture.device,
            responseProtocolVersion: RemoteControlProtocol.version + 1
        )

        do {
            _ = try await successfulClient(body: body).pair(
                payload: fixture.payload,
                device: fixture.device,
                now: fixture.now
            )
            XCTFail("expected protocol mismatch")
        } catch let error as RemotePairingClientError {
            XCTAssertEqual(error, .incompatibleProtocol)
        }
    }

    func testRejectsResponseForDifferentControllerIdentity() async throws {
        let fixture = try PairingFixture()
        let body = try sealedResponse(
            payload: fixture.payload,
            device: fixture.device,
            responseDeviceID: "another-controller"
        )

        do {
            _ = try await successfulClient(body: body).pair(
                payload: fixture.payload,
                device: fixture.device,
                now: fixture.now
            )
            XCTFail("expected Controller identity mismatch")
        } catch let error as RemotePairingClientError {
            XCTAssertEqual(error, .responseDeviceIdentityMismatch)
        }
    }

    func testRejectsMalformedCommandCredentials() async throws {
        let fixture = try PairingFixture()
        let bodies = try [
            sealedResponse(
                payload: fixture.payload,
                device: fixture.device,
                responseAuthToken: ""
            ),
            sealedResponse(
                payload: fixture.payload,
                device: fixture.device,
                responseRelayMacID: "another-host"
            ),
            sealedResponse(
                payload: fixture.payload,
                device: fixture.device,
                responseE2EKey: Data(repeating: 7, count: 31)
            ),
        ]

        for body in bodies {
            do {
                _ = try await successfulClient(body: body).pair(
                    payload: fixture.payload,
                    device: fixture.device,
                    now: fixture.now
                )
                XCTFail("expected malformed credentials to be rejected")
            } catch let error as RemotePairingClientError {
                XCTAssertEqual(error, .invalidCredentials)
            }
        }
    }

    func testPreservesHostErrorMessage() async throws {
        let fixture = try PairingFixture()
        let body = try JSONSerialization.data(withJSONObject: ["error": "pairing token expired"])
        let client = RemotePairingClient { request in
            (
                body,
                try XCTUnwrap(HTTPURLResponse(
                    url: request.url!,
                    statusCode: 401,
                    httpVersion: nil,
                    headerFields: nil
                ))
            )
        }

        do {
            _ = try await client.pair(
                payload: fixture.payload,
                device: fixture.device,
                now: fixture.now
            )
            XCTFail("expected HTTP failure")
        } catch let error as RemotePairingClientError {
            XCTAssertEqual(
                error,
                .httpStatus(statusCode: 401, serverMessage: "pairing token expired")
            )
        }
    }

    func testRejectsResponseReplayedFromAnotherPairingSecret() async throws {
        let fixture = try PairingFixture()
        let otherPayload = RemotePairingPayload(
            macID: fixture.payload.macID,
            macName: fixture.payload.macName,
            endpoint: fixture.payload.endpoint,
            token: "another-one-time-secret",
            expiresAtUnixMs: fixture.payload.expiresAtUnixMs
        )
        let replayedBody = try sealedResponse(payload: otherPayload, device: fixture.device)
        let client = successfulClient(body: replayedBody)

        do {
            _ = try await client.pair(
                payload: fixture.payload,
                device: fixture.device,
                now: fixture.now
            )
            XCTFail("expected replayed response to fail authentication")
        } catch let error as RemotePairingCryptoError {
            XCTAssertEqual(error, .authenticationFailed)
        }
    }
}

private actor RequestRecorder {
    private(set) var request: URLRequest?

    func record(_ request: URLRequest) {
        self.request = request
    }
}

private enum TestFailure: Error {
    case unexpectedRequest
}

private struct PairingFixture {
    let payload: RemotePairingPayload
    let device: RemoteDeviceIdentity
    let now = Date(timeIntervalSince1970: 1)

    init() throws {
        payload = RemotePairingPayload(
            macID: "host-1",
            macName: "Studio Host",
            endpoint: try XCTUnwrap(URL(string: "http://10.0.0.4:17661/mobile")),
            token: "one-time-secret",
            expiresAtUnixMs: 2_000
        )
        device = RemoteDeviceIdentity(
            id: "controller-1",
            name: "Controller",
            platform: "macOS"
        )
    }
}

private func successfulClient(body: Data) -> RemotePairingClient {
    RemotePairingClient { request in
        (
            body,
            try XCTUnwrap(HTTPURLResponse(
                url: request.url!,
                statusCode: 200,
                httpVersion: nil,
                headerFields: nil
            ))
        )
    }
}

private func sealedResponse(
    payload: RemotePairingPayload,
    device: RemoteDeviceIdentity,
    responseProtocolVersion: Int = RemoteControlProtocol.version,
    responseMacID: String? = nil,
    responseEndpoint: URL? = nil,
    responseDeviceID: String? = nil,
    responseAuthToken: String = "bearer-token",
    responseRelayMacID: String? = nil,
    responseE2EKey: Data = Data(repeating: 7, count: 32)
) throws -> Data {
    let response = RemotePairingResponse(
        protocolVersion: responseProtocolVersion,
        macID: responseMacID ?? payload.macID,
        macName: payload.macName,
        endpoint: responseEndpoint ?? payload.endpoint,
        deviceID: responseDeviceID ?? device.id,
        authToken: responseAuthToken,
        pairedAtUnixMs: 1_500,
        relayCredentials: RelayCredentials(
            relayURL: try XCTUnwrap(URL(string: "wss://relay.unpeel.test")),
            macID: responseRelayMacID ?? payload.macID,
            relayToken: "relay-token",
            e2eKey: responseE2EKey
        )
    )
    let plaintext = try JSONEncoder().encode(response)
    let envelope = try RemotePairingCrypto.seal(
        plaintext,
        token: payload.token,
        macID: payload.macID,
        endpoint: payload.endpoint,
        direction: .response
    )
    return try JSONEncoder().encode(envelope)
}
