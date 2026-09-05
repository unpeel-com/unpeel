import XCTest
@testable import UnpeelShared

/// The transport-level contract shared by both Apple Controllers: the
/// additive `serverVersion` / `host.mobile.tls` signals in bootstrap and the
/// sealed pairing response, the https-tolerant Direct endpoint check, and
/// the relay's request-expiry rule.
final class RemoteTransportContractTests: XCTestCase {
    func testBootstrapDecodesServerVersionAdditively() throws {
        let legacy = #"{"protocolVersion":1,"folders":[],"projects":[],"presets":[],"sessions":[],"capturedAtUnixMs":1}"#
        let decodedLegacy = try JSONDecoder().decode(
            RemoteBootstrapSnapshot.self,
            from: Data(legacy.utf8)
        )
        XCTAssertNil(decodedLegacy.serverVersion)

        let current = #"{"protocolVersion":1,"folders":[],"projects":[],"presets":[],"sessions":[],"capturedAtUnixMs":1,"serverVersion":"0.5.3","hostProtocol":{"majorVersion":1,"minorVersion":14,"capabilities":["host.mobile.tls"]}}"#
        let decoded = try JSONDecoder().decode(
            RemoteBootstrapSnapshot.self,
            from: Data(current.utf8)
        )
        XCTAssertEqual(decoded.serverVersion, "0.5.3")
        XCTAssertTrue(decoded.hostProtocol?.supports(RemoteControlProtocol.mobileTLSCapability) == true)
        XCTAssertEqual(RemoteControlProtocol.mobileTLSCapability, "host.mobile.tls")
        XCTAssertEqual(RemoteControlProtocol.mobileTLSMinimumServerVersion, "0.5.3")
    }

    func testPairingResponseDecodesServerVersionAdditively() throws {
        let json = #"{"protocolVersion":1,"macID":"mac-1","macName":"Studio","endpoint":"http://192.168.1.10:4485/mobile","deviceID":"device-1","authToken":"bearer","pairedAtUnixMs":1,"relayCredentials":{"relayURL":"wss://relay.example.test","macID":"mac-1","relayToken":"relay","e2eKeyB64":"QkJCQkJCQkJCQkJCQkJCQkJCQkJCQkJCQkJCQkJCQkI="},"serverVersion":"0.5.3"}"#
        let decoded = try JSONDecoder().decode(RemotePairingResponse.self, from: Data(json.utf8))
        XCTAssertEqual(decoded.serverVersion, "0.5.3")

        let withoutVersion = json.replacingOccurrences(of: #","serverVersion":"0.5.3""#, with: "")
        let legacy = try JSONDecoder().decode(
            RemotePairingResponse.self,
            from: Data(withoutVersion.utf8)
        )
        XCTAssertNil(legacy.serverVersion)
    }

    func testRelayRequestExpiryRetiresOnlyASilentSocket() {
        let sentAt = Date(timeIntervalSince1970: 1_000)
        // Inbound traffic after the send proves the path: never retire.
        XCTAssertFalse(RemoteRelayRequestExpiryPolicy.shouldRetireConnection(
            sentAt: sentAt,
            lastIncomingAt: sentAt.addingTimeInterval(1),
            now: sentAt.addingTimeInterval(60)
        ))
        // Silence since before the send, but within the keepalive cadence:
        // a slow Host on a live socket. Fail the request, keep the socket.
        XCTAssertFalse(RemoteRelayRequestExpiryPolicy.shouldRetireConnection(
            sentAt: sentAt,
            lastIncomingAt: sentAt.addingTimeInterval(-3),
            now: sentAt.addingTimeInterval(10)
        ))
        // Silence past the keepalive cadence: a black-holed socket.
        XCTAssertTrue(RemoteRelayRequestExpiryPolicy.shouldRetireConnection(
            sentAt: sentAt,
            lastIncomingAt: sentAt.addingTimeInterval(-12),
            now: sentAt.addingTimeInterval(10)
        ))
        XCTAssertEqual(RemoteRelayRequestExpiryPolicy.silenceLimit, 20)
    }
}
