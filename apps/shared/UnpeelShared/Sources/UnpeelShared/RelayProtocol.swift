//
//  RelayProtocol.swift
//  UnpeelShared
//
//  Wire protocol + end-to-end crypto for Unpeel Remote — the Cloudflare
//  relay that connects the phone to the Mac away from the LAN. Both the
//  Mac uplink and the iOS client use this file verbatim; the relay itself
//  never links it (it only pipes the opaque bytes these functions produce).
//
//  Security design (see docs/feature/unpeel-remote.md):
//  - Per-device static 32-byte e2eKey, exchanged at pairing over the LAN.
//  - Per-connection handshake: both sides contribute a fresh 16-byte salt;
//    session keys are HKDF-SHA256(e2eKey, saltC‖saltH, direction info) —
//    one AES-256-GCM key per direction.
//  - Nonces are 12 bytes: a 4-byte direction tag ‖ an 8-byte strictly
//    increasing counter. Receivers enforce monotonicity, so replayed or
//    reordered ciphertexts are rejected even though they'd decrypt.
//  - Any AEAD failure is terminal for the session: callers must drop the
//    connection, never skip a frame.
//

import CryptoKit
import Compression
import Foundation

public enum RelayProtocol {
    public static let version = 1
    /// Relay WS frame types (first byte of every relay frame).
    public enum FrameType: UInt8 {
        case hello = 0x01
        case data = 0x02
        case clientClosed = 0x03
        case clientData = 0x04
    }
    /// Cap matching the relay Worker's enforcement. This is the complete
    /// sealed payload, including the counter prefix and authentication tag.
    public static let maxFrameBytes = 512 * 1024
    /// `[counter u64 BE]` plus the 16-byte AES-GCM authentication tag.
    public static let aeadOverheadBytes = MemoryLayout<UInt64>.size + 16
    /// Largest plaintext that can be sealed into one accepted relay frame.
    public static let maxPlaintextBytes = maxFrameBytes - aeadOverheadBytes
}

// MARK: - Relay frame envelopes (host side of the DO protocol)

public enum RelayHostFrame {
    /// `[0x02][connID u32 BE][opaque]` — data to/from one phone connection.
    public static func encodeData(connID: UInt32, payload: Data) -> Data {
        var out = Data([RelayProtocol.FrameType.data.rawValue])
        out.append(contentsOf: withUnsafeBytes(of: connID.bigEndian) { Array($0) })
        out.append(payload)
        return out
    }

    public enum Incoming: Equatable {
        case clientClosed(connID: UInt32)
        case clientData(connID: UInt32, deviceID: String, payload: Data)
    }

    public static func decode(_ frame: Data) -> Incoming? {
        guard frame.count >= 5, let type = RelayProtocol.FrameType(rawValue: frame[frame.startIndex]) else {
            return nil
        }
        let connID = frame.dropFirst().prefix(4).reduce(UInt32(0)) { ($0 << 8) | UInt32($1) }
        switch type {
        case .data:
            return nil
        case .clientClosed:
            return .clientClosed(connID: connID)
        case .clientData:
            guard frame.count >= 7 else { return nil }
            let idLength = Int(frame[frame.startIndex + 5])
            guard idLength > 0, idLength <= 128, frame.count >= 6 + idLength,
                  let deviceID = String(data: Data(frame.dropFirst(6).prefix(idLength)), encoding: .utf8),
                  !deviceID.isEmpty else { return nil }
            return .clientData(
                connID: connID,
                deviceID: deviceID,
                payload: Data(frame.dropFirst(6 + idLength))
            )
        case .hello:
            return nil
        }
    }

    /// `[0x01][JSON]` — registers paired device IDs + relayToken hashes.
    public static func encodeHello(devices: [RelayDeviceTokenRegistration]) -> Data? {
        struct Hello: Encodable {
            let v: Int
            let devices: [RelayDeviceTokenRegistration]
        }
        guard let json = try? JSONEncoder().encode(
            Hello(v: RelayProtocol.version, devices: devices)
        ) else { return nil }
        return Data([RelayProtocol.FrameType.hello.rawValue]) + json
    }
}

public struct RelayDeviceTokenRegistration: Codable, Equatable, Sendable {
    public let deviceID: String
    public let tokenHash: String

    public init(deviceID: String, tokenHash: String) {
        self.deviceID = deviceID
        self.tokenHash = tokenHash
    }
}

// MARK: - E2E session crypto

public enum RelayCryptoError: Error, Equatable {
    case handshakeFailed
    case frameTooLarge
    case sealFailed
    case openFailed
    case replayDetected
    case counterExhausted
}

/// One direction of the encrypted channel. `seal` and `open` are stateful:
/// the send counter increments per frame, the receive side enforces strict
/// monotonicity. A single failure poisons the session (callers drop the
/// connection and re-handshake).
public struct RelayCryptoSession: Sendable {
    private let sendKey: SymmetricKey
    private let receiveKey: SymmetricKey
    private let sendDirectionTag: Data
    private let receiveDirectionTag: Data
    private var sendCounter: UInt64 = 0
    private var lastReceivedCounter: UInt64 = 0

    private static let clientTag = Data("c2h!".utf8)
    private static let hostTag = Data("h2c!".utf8)

    /// Derive the per-connection session. `isHost` selects which derived key
    /// encrypts which direction — both sides call this with identical inputs.
    ///
    /// The input keying material is the **static per-device key concatenated
    /// with a fresh ephemeral X25519 shared secret** (`RelayHandshake`), so
    /// each connection's keys depend on ephemeral material neither the relay
    /// nor a later static-key theft can reconstruct — forward secrecy. The
    /// ephemeral secret also means a replayed salt can never repeat a key,
    /// which closes any RNG-degraded nonce-reuse path.
    public init(
        e2eKey: Data,
        sharedSecret: Data,
        clientSalt: Data,
        hostSalt: Data,
        isHost: Bool
    ) throws {
        guard e2eKey.count == 32, sharedSecret.count == 32,
              clientSalt.count == 16, hostSalt.count == 16 else {
            throw RelayCryptoError.handshakeFailed
        }
        let ikm = SymmetricKey(data: e2eKey + sharedSecret)
        let salt = clientSalt + hostSalt
        func derive(_ info: String) -> SymmetricKey {
            HKDF<SHA256>.deriveKey(
                inputKeyMaterial: ikm,
                salt: salt,
                info: Data("unpeel-relay-v\(RelayProtocol.version):\(info)".utf8),
                outputByteCount: 32
            )
        }
        let clientToHost = derive("c2h")
        let hostToClient = derive("h2c")
        sendKey = isHost ? hostToClient : clientToHost
        receiveKey = isHost ? clientToHost : hostToClient
        sendDirectionTag = isHost ? Self.hostTag : Self.clientTag
        receiveDirectionTag = isHost ? Self.clientTag : Self.hostTag
    }

    private static func nonce(directionTag: Data, counter: UInt64) -> AES.GCM.Nonce {
        var bytes = directionTag
        bytes.append(contentsOf: withUnsafeBytes(of: counter.bigEndian) { Array($0) })
        // 4 + 8 = the 12 bytes AES.GCM.Nonce requires; cannot fail.
        return try! AES.GCM.Nonce(data: bytes)
    }

    /// Encrypt one frame. Output layout: `[counter u64 BE][GCM ciphertext+tag]`.
    public mutating func seal(_ plaintext: Data) throws -> Data {
        guard plaintext.count <= RelayProtocol.maxPlaintextBytes else {
            throw RelayCryptoError.frameTooLarge
        }
        guard sendCounter < .max else { throw RelayCryptoError.counterExhausted }
        sendCounter += 1
        let counter = sendCounter
        let nonce = Self.nonce(directionTag: sendDirectionTag, counter: counter)
        guard let box = try? AES.GCM.seal(plaintext, using: sendKey, nonce: nonce) else {
            throw RelayCryptoError.sealFailed
        }
        var out = Data(withUnsafeBytes(of: counter.bigEndian) { Array($0) })
        out.append(box.ciphertext)
        out.append(box.tag)
        return out
    }

    /// Decrypt one frame, enforcing the strictly-increasing counter. The
    /// counter is authenticated by construction: it *is* the nonce, so a
    /// forged counter makes the AEAD open fail.
    public mutating func open(_ frame: Data) throws -> Data {
        guard frame.count >= 8 + 16 else { throw RelayCryptoError.openFailed }
        let counter = frame.prefix(8).reduce(UInt64(0)) { ($0 << 8) | UInt64($1) }
        guard counter > lastReceivedCounter else { throw RelayCryptoError.replayDetected }
        let nonce = Self.nonce(directionTag: receiveDirectionTag, counter: counter)
        let body = frame.dropFirst(8)
        let ciphertext = body.dropLast(16)
        let tag = body.suffix(16)
        guard let box = try? AES.GCM.SealedBox(nonce: nonce, ciphertext: ciphertext, tag: tag),
              let plaintext = try? AES.GCM.open(box, using: receiveKey)
        else { throw RelayCryptoError.openFailed }
        lastReceivedCounter = counter
        return plaintext
    }
}

// MARK: - Handshake

/// Ephemeral-DH + static-key handshake helpers. The transcript MAC is keyed
/// by the static per-device key, so a relay that swaps the (unauthenticated)
/// plaintext ephemeral keys or downgrades the version is detected before any
/// sealed frame is accepted — the relay cannot MITM without the device key.
public enum RelayHandshake {
    public struct EphemeralKeyPair: Sendable {
        public let privateKey: Curve25519.KeyAgreement.PrivateKey
        public var publicKey: Data { privateKey.publicKey.rawRepresentation }
        public init() { privateKey = Curve25519.KeyAgreement.PrivateKey() }
    }

    /// X25519 raw shared secret between our ephemeral private key and the
    /// peer's ephemeral public key. 32 bytes; throws on a malformed peer key.
    public static func sharedSecret(
        privateKey: Curve25519.KeyAgreement.PrivateKey,
        peerPublicKey: Data
    ) throws -> Data {
        let peer = try Curve25519.KeyAgreement.PublicKey(rawRepresentation: peerPublicKey)
        let secret = try privateKey.sharedSecretFromKeyAgreement(with: peer)
        return secret.withUnsafeBytes { Data($0) }
    }

    /// HMAC-SHA256 over the length-prefixed handshake transcript, keyed by a
    /// dedicated key derived from the static device key. Both sides compute
    /// it over identical inputs and compare constant-time.
    public static func transcriptMAC(
        e2eKey: Data,
        deviceID: String,
        clientSalt: Data,
        hostSalt: Data,
        clientEphemeralPublicKey: Data,
        hostEphemeralPublicKey: Data
    ) -> Data {
        let macKey = HKDF<SHA256>.deriveKey(
            inputKeyMaterial: SymmetricKey(data: e2eKey),
            info: Data("unpeel-relay-v\(RelayProtocol.version):handshake-mac".utf8),
            outputByteCount: 32
        )
        var transcript = Data()
        transcript.append(contentsOf: withUnsafeBytes(of: UInt32(RelayProtocol.version).bigEndian) { Array($0) })
        for field in [Data(deviceID.utf8), clientSalt, hostSalt, clientEphemeralPublicKey, hostEphemeralPublicKey] {
            // Length-prefix every field so a variable-length deviceID can't
            // shift bytes into an adjacent field.
            transcript.append(contentsOf: withUnsafeBytes(of: UInt32(field.count).bigEndian) { Array($0) })
            transcript.append(field)
        }
        let mac = HMAC<SHA256>.authenticationCode(for: transcript, using: macKey)
        return Data(mac)
    }

    /// Constant-time equality for the MAC comparison.
    public static func constantTimeEqual(_ a: Data, _ b: Data) -> Bool {
        guard a.count == b.count else { return false }
        var diff: UInt8 = 0
        for (x, y) in zip(a, b) { diff |= x ^ y }
        return diff == 0
    }
}

// MARK: - Handshake messages (the only plaintext the tunnel ever carries)

/// Client's first opaque payload: which device key to use, its salt, and its
/// ephemeral public key. Public values only — worthless to the relay without
/// the static device key.
public struct RelayClientHello: Codable, Equatable, Sendable {
    public let v: Int
    public let deviceID: String
    public let saltB64: String
    public let ephemeralPublicKeyB64: String

    public init(deviceID: String, salt: Data, ephemeralPublicKey: Data) {
        v = RelayProtocol.version
        self.deviceID = deviceID
        saltB64 = salt.base64EncodedString()
        ephemeralPublicKeyB64 = ephemeralPublicKey.base64EncodedString()
    }

    public var salt: Data? {
        Data(base64Encoded: saltB64)
    }

    public var ephemeralPublicKey: Data? {
        Data(base64Encoded: ephemeralPublicKeyB64)
    }
}

/// Host's reply: its salt, its ephemeral public key, and the transcript MAC
/// proving it holds the device key (and pinning both ephemeral keys against a
/// relay swap). After this, everything is sealed.
public struct RelayHostHello: Codable, Equatable, Sendable {
    public let v: Int
    public let saltB64: String
    public let ephemeralPublicKeyB64: String
    public let macB64: String

    public init(salt: Data, ephemeralPublicKey: Data, mac: Data) {
        v = RelayProtocol.version
        saltB64 = salt.base64EncodedString()
        ephemeralPublicKeyB64 = ephemeralPublicKey.base64EncodedString()
        macB64 = mac.base64EncodedString()
    }

    public var salt: Data? {
        Data(base64Encoded: saltB64)
    }

    public var ephemeralPublicKey: Data? {
        Data(base64Encoded: ephemeralPublicKeyB64)
    }

    public var mac: Data? {
        Data(base64Encoded: macB64)
    }
}

// MARK: - Tunneled /mobile requests (inside the encrypted channel)

/// One `/mobile/*` request tunneled through the relay. Ids correlate
/// concurrent requests over one connection (bootstrap + output long-poll).
public struct RelayTunnelRequest: Codable, Equatable, Sendable {
    public let id: UInt64
    public let method: String
    /// Full path including the `/mobile` prefix, e.g. `/mobile/bootstrap`.
    public let path: String
    public let query: [String: String]
    /// Complete Authorization value (normally `Bearer <device token>`) — the
    /// same header value the LAN path sends; the Host verifies it identically.
    public let auth: String?
    /// Needed by routes that read it (image upload's png/jpg sniff).
    public let contentType: String?
    public let bodyB64: String?

    public init(
        id: UInt64,
        method: String,
        path: String,
        query: [String: String] = [:],
        auth: String?,
        contentType: String? = nil,
        body: Data? = nil
    ) {
        self.id = id
        self.method = method
        self.path = path
        self.query = query
        self.auth = auth
        self.contentType = contentType
        bodyB64 = body?.base64EncodedString()
    }

    public var body: Data {
        bodyB64.flatMap { Data(base64Encoded: $0) } ?? Data()
    }
}

/// Canonical request encoding at the relay's encrypted-frame boundary.
/// Keeping this beside the wire model ensures every sender measures the
/// complete JSON/base64 envelope, not merely the raw request body.
public enum RelayTunnelCodec {
    public static func encodeRequest(_ request: RelayTunnelRequest) throws -> Data {
        let plaintext = try JSONEncoder().encode(request)
        guard plaintext.count <= RelayProtocol.maxPlaintextBytes else {
            throw URLError(.dataLengthExceedsMaximum)
        }
        return plaintext
    }
}

public struct RelayTunnelResponse: Codable, Equatable, Sendable {
    public let id: UInt64
    public let status: Int
    public let bodyB64: String?

    public init(id: UInt64, status: Int, body: Data?) {
        self.id = id
        self.status = status
        bodyB64 = body?.base64EncodedString()
    }

    public var body: Data {
        bodyB64.flatMap { Data(base64Encoded: $0) } ?? Data()
    }
}

// MARK: - Relay output push (host→phone terminal stream)

/// Paths for the relay-only output stream, intercepted by the Mac's relay
/// uplink BEFORE the tunneled `/mobile/*` pipeline. Subscribing turns the
/// tunnel from request/response polling into host push: the Mac streams
/// output frames as they are produced, so cellular latency is one-way
/// instead of a full round-trip per update. Deliberately outside
/// `/mobile/`: an older Mac's tunnel pipeline 404s the subscribe, which is
/// the phone's signal to fall back to long-polling.
public enum RelayStreamPaths {
    public static let subscribe = "/relay/output-stream"
    public static let credit = "/relay/output-credit"
    public static let unsubscribe = "/relay/output-stream-stop"
    /// A renderer-generation nonce carried by every stream control request.
    /// Older Hosts ignore the additive query item; current Hosts bind credit,
    /// stop, and pushed frames to the exact subscription that created them.
    public static let subscriptionIDQuery = "subscription_id"
}

/// A terminal event for a Relay output subscription. Ordinary output keeps
/// this field absent for wire compatibility. A terminal event lets the phone
/// leave an otherwise-open shared Relay socket and fall back/reconnect when
/// the Host-side stream itself ends after its first frame.
public enum RelayStreamEvent: String, Codable, Equatable, Sendable {
    case ended
    case error
}

/// One pushed terminal-output frame. Rides the same per-connection crypto
/// session as tunnel responses (same direction, same nonce counter — the
/// host serializes all sends, so ordering holds). Distinguished from
/// `RelayTunnelResponse` by shape: responses always carry `id` + `status`,
/// pushes always carry `stream` + `offset`.
public struct RelayStreamPush: Codable, Equatable, Sendable {
    /// Session id this frame belongs to (the phone ignores frames for
    /// sessions it no longer views).
    public let stream: String
    /// Exact renderer subscription this frame belongs to. Nil is accepted
    /// only for compatibility with a Host predating subscription identities.
    public let subscriptionID: String?
    /// Absolute `output.bin` offset of the first payload byte.
    public let offset: UInt64
    public let dataB64: String?
    /// Server replayed from a new position (tail replay on subscribe,
    /// truncated log, stale offset): the phone clears before feeding.
    public let rebased: Bool?
    /// Current PTY grid, piggybacked so the phone never needs a separate
    /// metrics round-trip while streaming. Sent on the first frame and
    /// whenever it changes.
    public let cols: Int?
    public let rows: Int?
    /// Nil for output frames (and every legacy Host frame). `ended`/`error`
    /// are explicit stream-terminal events; the shared Relay socket remains
    /// connected for bootstrap and other requests.
    public let event: RelayStreamEvent?
    /// Bounded diagnostic for an `error` event. Never shown as terminal data.
    public let message: String?
    /// Present only during a fresh relay attach. The Mac compresses one
    /// complete replay window, splits the compressed bytes into bounded
    /// parts, and the phone paints only after the final part arrives. The
    /// Durable Object remains an opaque frame pipe.
    public let bootstrap: RelayStreamBootstrapPart?

    public init(
        stream: String,
        subscriptionID: String? = nil,
        offset: UInt64,
        data: Data?,
        rebased: Bool? = nil,
        cols: Int? = nil,
        rows: Int? = nil,
        event: RelayStreamEvent? = nil,
        message: String? = nil,
        bootstrap: RelayStreamBootstrapPart? = nil
    ) {
        self.stream = stream
        self.subscriptionID = subscriptionID
        self.offset = offset
        dataB64 = data?.base64EncodedString()
        self.rebased = rebased
        self.cols = cols
        self.rows = rows
        self.event = event
        self.message = message
        self.bootstrap = bootstrap
    }

    public var data: Data {
        dataB64.flatMap { Data(base64Encoded: $0) } ?? Data()
    }
}

public enum RelayBootstrapEncoding: String, Codable, Sendable {
    case identity
    case lzfse
}

public struct RelayStreamBootstrapPart: Codable, Equatable, Sendable {
    public let index: Int
    public let final: Bool
    public let encoding: RelayBootstrapEncoding
    public let uncompressedBytes: Int
    /// Absolute output.bin offset immediately after the complete replay.
    public let endOffset: UInt64

    public init(
        index: Int,
        final: Bool,
        encoding: RelayBootstrapEncoding,
        uncompressedBytes: Int,
        endOffset: UInt64
    ) {
        self.index = index
        self.final = final
        self.encoding = encoding
        self.uncompressedBytes = uncompressedBytes
        self.endOffset = endOffset
    }
}

/// Apple-to-Apple bootstrap compression. Compression happens before E2E
/// encryption on the Mac and decompression after decryption on the phone, so
/// Cloudflare sees fewer opaque bytes and usually only one WebSocket message.
public enum RelayBootstrapCodec {
    public static let maximumUncompressedBytes = 8 * 1024 * 1024

    public static func encode(_ input: Data) -> (RelayBootstrapEncoding, Data) {
        guard !input.isEmpty else { return (.identity, input) }
        // LZFSE can expand incompressible input slightly. A 2x scratch buffer
        // is bounded by the replay cap; identity wins unless compression is
        // actually smaller on the wire.
        var output = Data(count: input.count * 2)
        let written = output.withUnsafeMutableBytes { destination in
            input.withUnsafeBytes { source in
                compression_encode_buffer(
                    destination.bindMemory(to: UInt8.self).baseAddress!,
                    destination.count,
                    source.bindMemory(to: UInt8.self).baseAddress!,
                    source.count,
                    nil,
                    COMPRESSION_LZFSE
                )
            }
        }
        guard written > 0, written < input.count else { return (.identity, input) }
        output.count = written
        return (.lzfse, output)
    }

    public static func decode(
        _ encoded: Data,
        encoding: RelayBootstrapEncoding,
        uncompressedBytes: Int
    ) -> Data? {
        guard uncompressedBytes >= 0,
              uncompressedBytes <= maximumUncompressedBytes else { return nil }
        switch encoding {
        case .identity:
            return encoded.count == uncompressedBytes ? encoded : nil
        case .lzfse:
            guard uncompressedBytes > 0, !encoded.isEmpty else { return nil }
            // One extra byte distinguishes an exact decode from a truncated
            // decode into an undersized destination (Compression otherwise
            // reports the destination capacity as a plausible byte count).
            var output = Data(count: uncompressedBytes + 1)
            let written = output.withUnsafeMutableBytes { destination in
                encoded.withUnsafeBytes { source in
                    compression_decode_buffer(
                        destination.bindMemory(to: UInt8.self).baseAddress!,
                        destination.count,
                        source.bindMemory(to: UInt8.self).baseAddress!,
                        source.count,
                        nil,
                        COMPRESSION_LZFSE
                    )
                }
            }
            guard written == uncompressedBytes else { return nil }
            output.count = uncompressedBytes
            return output
        }
    }
}

// MARK: - Relay credentials (pairing response / /mobile/relay-credentials)

/// What a phone needs to reach its Mac through the relay. e2eKey and
/// relayToken are Keychain-only on the phone; the Mac stores the raw e2eKey
/// and only the SHA-256 of relayToken.
public struct RelayCredentials: Codable, Equatable, Sendable {
    public let relayURL: URL
    public let macID: String
    public let relayToken: String
    public let e2eKeyB64: String

    public init(relayURL: URL, macID: String, relayToken: String, e2eKey: Data) {
        self.relayURL = relayURL
        self.macID = macID
        self.relayToken = relayToken
        e2eKeyB64 = e2eKey.base64EncodedString()
    }

    public var e2eKey: Data? {
        Data(base64Encoded: e2eKeyB64)
    }
}
