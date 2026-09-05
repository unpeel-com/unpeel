import Foundation
import XCTest
import UnpeelShared
@testable import UnpeelNative

private final class ControllerProxyE2EKeyStore: MobileE2EKeyStoring {
    private var keys: [String: Data] = [:]

    func load(deviceID: String) -> Data? { keys[deviceID] }
    func save(_ key: Data, deviceID: String) throws { keys[deviceID] = key }
    func delete(deviceID: String) { keys[deviceID] = nil }
}

@MainActor
final class ControllerPairingProxyTests: XCTestCase {
    func testClientOnlyControllerCompletesSealedRemotePhonePairingOnce() async throws {
        // The app is a pure Controller: the proxy below has no Host routes
        // and proves controller-assisted pairing needs none.
        let proxy = try XCTUnwrap(ControllerPairingProxy(advertisedHost: "127.0.0.1"))
        defer { proxy.stop() }
        let hostStore = MobilePairingStore(
            storageURL: scratchURL().appendingPathComponent("devices.json"),
            macID: "remote-host",
            macName: "Remote Host",
            e2eKeyStore: ControllerProxyE2EKeyStore()
        )
        let directEndpoint = try XCTUnwrap(URL(string: "http://10.0.0.8:17661/mobile"))
        var forwardedEnvelopeCount = 0
        let reservation = try XCTUnwrap(proxy.reserve { body in
            forwardedEnvelopeCount += 1
            let envelope = try JSONDecoder().decode(RemotePairingEnvelope.self, from: body)
            let request = try hostStore.decryptPairingRequest(envelope)
            let response = try hostStore.pair(request)
            let plaintext = try JSONEncoder().encode(response)
            let sealed = try RemotePairingCrypto.seal(
                plaintext,
                token: request.token,
                macID: response.macID,
                endpoint: response.endpoint,
                direction: .response
            )
            return try JSONEncoder().encode(sealed)
        })
        let payload = hostStore.beginPairing(
            endpoint: reservation.endpoint,
            directEndpoint: directEndpoint
        )
        let device = RemoteDeviceIdentity(
            id: "phone-1",
            name: "iPhone",
            platform: "iOS"
        )

        let client = RemotePairingClient(session: loopbackSession())
        let paired = try await client.pair(payload: payload, device: device)

        XCTAssertEqual(forwardedEnvelopeCount, 1)
        XCTAssertEqual(paired.macID, "remote-host")
        XCTAssertEqual(paired.endpoint, reservation.endpoint)
        XCTAssertEqual(paired.directEndpoint, directEndpoint)
        XCTAssertEqual(
            hostStore.verifyAuthorizationHeader("Bearer \(paired.authToken)"),
            device.id
        )

        do {
            _ = try await client.pair(payload: payload, device: device)
            XCTFail("the one-shot proxy must reject a second exchange")
        } catch let error as RemotePairingClientError {
            XCTAssertEqual(
                error,
                .httpStatus(
                    statusCode: 410,
                    serverMessage: "pairing invitation expired"
                )
            )
        }
        XCTAssertEqual(forwardedEnvelopeCount, 1)
    }

    func testControllerProxyExposesNoHostRoutes() async throws {
        let proxy = try XCTUnwrap(ControllerPairingProxy(advertisedHost: "127.0.0.1"))
        defer { proxy.stop() }
        let reservation = try XCTUnwrap(proxy.reserve { _ in Data("{}".utf8) })
        var components = try XCTUnwrap(URLComponents(
            url: reservation.endpoint,
            resolvingAgainstBaseURL: false
        ))
        components.path = "/mobile/bootstrap"
        let url = try XCTUnwrap(components.url)
        var request = URLRequest(url: url)
        request.httpMethod = "POST"
        request.httpBody = Data("{}".utf8)
        let (data, response) = try await loopbackSession().data(for: request)

        XCTAssertEqual((response as? HTTPURLResponse)?.statusCode, 404)
        XCTAssertEqual(
            try JSONSerialization.jsonObject(with: data) as? [String: String],
            ["error": "not found"]
        )
    }

    private func loopbackSession() -> URLSession {
        let configuration = URLSessionConfiguration.ephemeral
        configuration.connectionProxyDictionary = [:]
        return URLSession(configuration: configuration)
    }

    private func scratchURL() -> URL {
        let url = FileManager.default.temporaryDirectory
            .appendingPathComponent("unpeel-controller-proxy-\(UUID().uuidString)")
        addTeardownBlock { try? FileManager.default.removeItem(at: url) }
        return url
    }
}
