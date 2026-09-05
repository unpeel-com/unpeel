import CryptoKit
import Foundation
import XCTest
@testable import UnpeelShared

final class RelayProtocolTests: XCTestCase {
    private func makeSessions(
        key: Data = Data((0 ..< 32).map { UInt8($0) }),
        saltC: Data = Data(repeating: 0xAA, count: 16),
        saltH: Data = Data(repeating: 0xBB, count: 16)
    ) throws -> (client: RelayCryptoSession, host: RelayCryptoSession) {
        // A real ephemeral DH: both sides agree on the same shared secret.
        let clientEph = RelayHandshake.EphemeralKeyPair()
        let hostEph = RelayHandshake.EphemeralKeyPair()
        let clientSecret = try RelayHandshake.sharedSecret(
            privateKey: clientEph.privateKey,
            peerPublicKey: hostEph.publicKey
        )
        let hostSecret = try RelayHandshake.sharedSecret(
            privateKey: hostEph.privateKey,
            peerPublicKey: clientEph.publicKey
        )
        XCTAssertEqual(clientSecret, hostSecret, "X25519 must agree")
        let client = try RelayCryptoSession(
            e2eKey: key, sharedSecret: clientSecret,
            clientSalt: saltC, hostSalt: saltH, isHost: false
        )
        let host = try RelayCryptoSession(
            e2eKey: key, sharedSecret: hostSecret,
            clientSalt: saltC, hostSalt: saltH, isHost: true
        )
        return (client, host)
    }

    func testSealOpenRoundTripBothDirections() throws {
        var (client, host) = try makeSessions()
        let up = Data("ls -la\n".utf8)
        let down = Data("total 42".utf8)

        let sealedUp = try client.seal(up)
        XCTAssertNotEqual(sealedUp, up)
        XCTAssertEqual(try host.open(sealedUp), up)

        let sealedDown = try host.seal(down)
        XCTAssertEqual(try client.open(sealedDown), down)
    }

    func testRelayFrameSizeContractAllowsExactPlaintextBoundary() throws {
        XCTAssertEqual(RelayProtocol.aeadOverheadBytes, 24)
        XCTAssertEqual(
            RelayProtocol.maxPlaintextBytes + RelayProtocol.aeadOverheadBytes,
            RelayProtocol.maxFrameBytes
        )

        var (client, host) = try makeSessions()
        let plaintext = Data(repeating: 0xA5, count: RelayProtocol.maxPlaintextBytes)
        let sealed = try client.seal(plaintext)
        XCTAssertEqual(sealed.count, RelayProtocol.maxFrameBytes)
        XCTAssertEqual(try host.open(sealed), plaintext)
    }

    func testReplayIsRejected() throws {
        var (client, host) = try makeSessions()
        let sealed = try client.seal(Data("rm -rf /\n".utf8))
        _ = try host.open(sealed)
        XCTAssertThrowsError(try host.open(sealed)) { error in
            XCTAssertEqual(error as? RelayCryptoError, .replayDetected)
        }
    }

    func testOutOfOrderOldFrameIsRejected() throws {
        var (client, host) = try makeSessions()
        let first = try client.seal(Data("a".utf8))
        let second = try client.seal(Data("b".utf8))
        _ = try host.open(second)
        XCTAssertThrowsError(try host.open(first)) { error in
            XCTAssertEqual(error as? RelayCryptoError, .replayDetected)
        }
    }

    func testTamperedCiphertextIsRejected() throws {
        var (client, host) = try makeSessions()
        var sealed = try client.seal(Data("hello".utf8))
        sealed[sealed.count - 1] ^= 0x01
        XCTAssertThrowsError(try host.open(sealed)) { error in
            XCTAssertEqual(error as? RelayCryptoError, .openFailed)
        }
    }

    func testForgedCounterIsRejected() throws {
        var (client, host) = try makeSessions()
        var sealed = try client.seal(Data("hello".utf8))
        // Bump the plaintext counter prefix without re-encrypting: the
        // nonce no longer matches the one the tag was computed under.
        sealed[7] = 9
        XCTAssertThrowsError(try host.open(sealed))
    }

    func testReflectionIsRejected() throws {
        var (client, _) = try makeSessions()
        let sealed = try client.seal(Data("hello".utf8))
        // A frame reflected back at its sender must not decrypt: the
        // receive direction uses the other derived key.
        XCTAssertThrowsError(try client.open(sealed)) { error in
            XCTAssertEqual(error as? RelayCryptoError, .openFailed)
        }
    }

    func testDifferentSaltsProduceIncompatibleSessions() throws {
        let key = Data(repeating: 7, count: 32)
        let secret = Data(repeating: 9, count: 32)
        var clientA = try RelayCryptoSession(
            e2eKey: key, sharedSecret: secret,
            clientSalt: Data(repeating: 1, count: 16),
            hostSalt: Data(repeating: 2, count: 16),
            isHost: false
        )
        var hostB = try RelayCryptoSession(
            e2eKey: key, sharedSecret: secret,
            clientSalt: Data(repeating: 1, count: 16),
            hostSalt: Data(repeating: 3, count: 16),
            isHost: true
        )
        let sealed = try clientA.seal(Data("hello".utf8))
        XCTAssertThrowsError(try hostB.open(sealed))
    }

    func testDifferentSharedSecretsProduceIncompatibleSessions() throws {
        // Forward secrecy: even identical static key + salts yield unopenable
        // traffic when the ephemeral DH secret differs (mismatched ephemerals
        // / a MITM-swapped ephemeral key).
        let key = Data(repeating: 7, count: 32)
        let saltC = Data(repeating: 1, count: 16)
        let saltH = Data(repeating: 2, count: 16)
        var client = try RelayCryptoSession(
            e2eKey: key, sharedSecret: Data(repeating: 4, count: 32),
            clientSalt: saltC, hostSalt: saltH, isHost: false
        )
        var host = try RelayCryptoSession(
            e2eKey: key, sharedSecret: Data(repeating: 5, count: 32),
            clientSalt: saltC, hostSalt: saltH, isHost: true
        )
        let sealed = try client.seal(Data("secret".utf8))
        XCTAssertThrowsError(try host.open(sealed))
    }

    func testSessionRejectsMalformedKeyMaterial() {
        let secret = Data(repeating: 9, count: 32)
        XCTAssertThrowsError(try RelayCryptoSession(
            e2eKey: Data(repeating: 1, count: 16), sharedSecret: secret,
            clientSalt: Data(repeating: 2, count: 16),
            hostSalt: Data(repeating: 3, count: 16),
            isHost: false
        ))
        XCTAssertThrowsError(try RelayCryptoSession(
            e2eKey: Data(repeating: 1, count: 32), sharedSecret: Data(repeating: 9, count: 16),
            clientSalt: Data(repeating: 2, count: 16),
            hostSalt: Data(repeating: 3, count: 16),
            isHost: false
        ))
        XCTAssertThrowsError(try RelayCryptoSession(
            e2eKey: Data(repeating: 1, count: 32), sharedSecret: secret,
            clientSalt: Data(),
            hostSalt: Data(repeating: 3, count: 16),
            isHost: false
        ))
    }

    func testTranscriptMACDetectsTamperingAndAuthenticatesPeer() throws {
        let key = Data((0 ..< 32).map { UInt8($0) })
        let deviceID = "phone-1"
        let saltC = Data(repeating: 1, count: 16)
        let saltH = Data(repeating: 2, count: 16)
        let clientEph = RelayHandshake.EphemeralKeyPair().publicKey
        let hostEph = RelayHandshake.EphemeralKeyPair().publicKey

        let mac = RelayHandshake.transcriptMAC(
            e2eKey: key, deviceID: deviceID,
            clientSalt: saltC, hostSalt: saltH,
            clientEphemeralPublicKey: clientEph, hostEphemeralPublicKey: hostEph
        )
        // Recomputing with the same inputs matches (client verifies host).
        let recomputed = RelayHandshake.transcriptMAC(
            e2eKey: key, deviceID: deviceID,
            clientSalt: saltC, hostSalt: saltH,
            clientEphemeralPublicKey: clientEph, hostEphemeralPublicKey: hostEph
        )
        XCTAssertTrue(RelayHandshake.constantTimeEqual(mac, recomputed))

        // A relay that swaps the host's ephemeral key is caught.
        let swapped = RelayHandshake.transcriptMAC(
            e2eKey: key, deviceID: deviceID,
            clientSalt: saltC, hostSalt: saltH,
            clientEphemeralPublicKey: clientEph,
            hostEphemeralPublicKey: RelayHandshake.EphemeralKeyPair().publicKey
        )
        XCTAssertFalse(RelayHandshake.constantTimeEqual(mac, swapped))

        // Wrong device key can't forge the MAC (proves peer holds e2eKey).
        let wrongKey = RelayHandshake.transcriptMAC(
            e2eKey: Data(repeating: 0xFF, count: 32), deviceID: deviceID,
            clientSalt: saltC, hostSalt: saltH,
            clientEphemeralPublicKey: clientEph, hostEphemeralPublicKey: hostEph
        )
        XCTAssertFalse(RelayHandshake.constantTimeEqual(mac, wrongKey))
    }

    func testEphemeralSharedSecretRejectsMalformedPeerKey() {
        let eph = RelayHandshake.EphemeralKeyPair()
        XCTAssertThrowsError(
            try RelayHandshake.sharedSecret(privateKey: eph.privateKey, peerPublicKey: Data([1, 2, 3]))
        )
    }

    func testHostOutboundDataIsNotAcceptedAsClientData() {
        let payload = Data("opaque".utf8)
        let frame = RelayHostFrame.encodeData(connID: 0x0102_0304, payload: payload)
        XCTAssertEqual(frame.first, RelayProtocol.FrameType.data.rawValue)
        XCTAssertNil(RelayHostFrame.decode(frame))
    }

    func testHostFrameDecodeClientClosed() {
        var frame = Data([RelayProtocol.FrameType.clientClosed.rawValue])
        frame.append(contentsOf: [0, 0, 0, 9])
        XCTAssertEqual(RelayHostFrame.decode(frame), .clientClosed(connID: 9))
    }

    func testHostFrameDecodeClientDataBindsDevice() {
        var frame = Data([RelayProtocol.FrameType.clientData.rawValue])
        frame.append(contentsOf: [0, 0, 0, 9])
        frame.append(UInt8("phone-1".utf8.count))
        frame.append(Data("phone-1".utf8))
        frame.append(Data("hello".utf8))
        XCTAssertEqual(
            RelayHostFrame.decode(frame),
            .clientData(connID: 9, deviceID: "phone-1", payload: Data("hello".utf8))
        )
    }

    func testHostFrameDecodeRejectsGarbage() {
        XCTAssertNil(RelayHostFrame.decode(Data()))
        XCTAssertNil(RelayHostFrame.decode(Data([0x7F, 0, 0, 0, 1])))
        XCTAssertNil(RelayHostFrame.decode(Data([RelayProtocol.FrameType.data.rawValue, 0])))
    }

    func testTunnelRequestResponseRoundTrip() throws {
        let binaryBody = Data([0x89, 0x50, 0x4E, 0x47, 0x00, 0xFF])
        let request = RelayTunnelRequest(
            id: 42,
            method: "POST",
            path: "/mobile/write",
            query: ["session_id": "s1"],
            auth: "Bearer token",
            contentType: "image/png",
            body: binaryBody
        )
        let decodedRequest = try JSONDecoder().decode(
            RelayTunnelRequest.self,
            from: JSONEncoder().encode(request)
        )
        XCTAssertEqual(decodedRequest, request)
        XCTAssertEqual(decodedRequest.auth, "Bearer token")
        XCTAssertEqual(decodedRequest.contentType, "image/png")
        XCTAssertEqual(decodedRequest.body, binaryBody)

        let response = RelayTunnelResponse(id: 42, status: 200, body: Data("{}".utf8))
        let decodedResponse = try JSONDecoder().decode(
            RelayTunnelResponse.self,
            from: JSONEncoder().encode(response)
        )
        XCTAssertEqual(decodedResponse, response)
    }

    func testTunnelCodecAcceptsExactBoundaryAndRejectsOneByteOver() throws {
        func request(encodedSize target: Int) throws -> RelayTunnelRequest {
            let base = RelayTunnelRequest(
                id: 1,
                method: "POST",
                path: "/mobile/write",
                query: ["padding": ""],
                auth: "Bearer token"
            )
            let baseSize = try JSONEncoder().encode(base).count
            XCTAssertGreaterThanOrEqual(target, baseSize)
            return RelayTunnelRequest(
                id: 1,
                method: "POST",
                path: "/mobile/write",
                query: ["padding": String(repeating: "x", count: target - baseSize)],
                auth: "Bearer token"
            )
        }

        let exact = try request(encodedSize: RelayProtocol.maxPlaintextBytes)
        let plaintext = try RelayTunnelCodec.encodeRequest(exact)
        XCTAssertEqual(plaintext.count, RelayProtocol.maxPlaintextBytes)

        var (client, host) = try makeSessions()
        let exactFrame = try client.seal(plaintext)
        XCTAssertEqual(exactFrame.count, RelayProtocol.maxFrameBytes)
        XCTAssertEqual(try host.open(exactFrame), plaintext)

        let oversized = try request(encodedSize: RelayProtocol.maxPlaintextBytes + 1)
        XCTAssertThrowsError(try RelayTunnelCodec.encodeRequest(oversized)) { error in
            XCTAssertEqual((error as? URLError)?.code, .dataLengthExceedsMaximum)
        }

        // `seal` is also defensive: an oversized plaintext is rejected before
        // the send counter advances, so the next accepted frame is counter 1.
        var (freshClient, freshHost) = try makeSessions()
        XCTAssertThrowsError(
            try freshClient.seal(Data(count: RelayProtocol.maxPlaintextBytes + 1))
        ) { error in
            XCTAssertEqual(error as? RelayCryptoError, .frameTooLarge)
        }
        let firstFrame = try freshClient.seal(Data("ok".utf8))
        let firstCounter = firstFrame.prefix(8).reduce(UInt64(0)) {
            ($0 << 8) | UInt64($1)
        }
        XCTAssertEqual(firstCounter, 1)
        XCTAssertEqual(try freshHost.open(firstFrame), Data("ok".utf8))
    }

    func testRelayBootstrapCompressesAndRoundTripsTerminalReplay() throws {
        let line = Data("\u{1B}[2J\u{1B}[Hterminal row with repeated styled content\r\n".utf8)
        var replay = Data()
        while replay.count < 768 * 1024 { replay.append(line) }

        let (encoding, encoded) = RelayBootstrapCodec.encode(replay)
        XCTAssertEqual(encoding, .lzfse)
        XCTAssertLessThan(encoded.count, replay.count / 10)
        XCTAssertEqual(
            RelayBootstrapCodec.decode(
                encoded,
                encoding: encoding,
                uncompressedBytes: replay.count
            ),
            replay
        )
        XCTAssertNil(
            RelayBootstrapCodec.decode(
                encoded,
                encoding: encoding,
                uncompressedBytes: replay.count - 1
            )
        )
    }

    func testRelayBootstrapMetadataRoundTrips() throws {
        let push = RelayStreamPush(
            stream: "session-1",
            offset: 100,
            data: Data("compressed".utf8),
            rebased: true,
            cols: 80,
            rows: 24,
            bootstrap: RelayStreamBootstrapPart(
                index: 0,
                final: true,
                encoding: .lzfse,
                uncompressedBytes: 768 * 1024,
                endOffset: 900
            )
        )
        XCTAssertEqual(
            try JSONDecoder().decode(
                RelayStreamPush.self,
                from: JSONEncoder().encode(push)
            ),
            push
        )
    }

    func testHandshakeMessagesRoundTrip() throws {
        let clientEph = RelayHandshake.EphemeralKeyPair().publicKey
        let clientHello = RelayClientHello(
            deviceID: "phone-1",
            salt: Data(repeating: 5, count: 16),
            ephemeralPublicKey: clientEph
        )
        let decoded = try JSONDecoder().decode(
            RelayClientHello.self,
            from: JSONEncoder().encode(clientHello)
        )
        XCTAssertEqual(decoded.salt, Data(repeating: 5, count: 16))
        XCTAssertEqual(decoded.ephemeralPublicKey, clientEph)

        let hostHello = RelayHostHello(
            salt: Data(repeating: 6, count: 16),
            ephemeralPublicKey: RelayHandshake.EphemeralKeyPair().publicKey,
            mac: Data(repeating: 7, count: 32)
        )
        let decodedHost = try JSONDecoder().decode(
            RelayHostHello.self,
            from: JSONEncoder().encode(hostHello)
        )
        XCTAssertEqual(decodedHost.salt, Data(repeating: 6, count: 16))
        XCTAssertEqual(decodedHost.mac, Data(repeating: 7, count: 32))
    }
}
