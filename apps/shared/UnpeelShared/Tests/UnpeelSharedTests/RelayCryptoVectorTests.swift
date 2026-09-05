import CryptoKit
import Foundation
import XCTest
@testable import UnpeelShared

/// Emits deterministic known-answer vectors for the relay E2E crypto and
/// checks them against hard-coded expected outputs. The SAME vectors are
/// reproduced independently by the WebCrypto implementation in
/// apps/relay/test/kat.test.mjs — if Swift and JS ever diverge, a phone and
/// Mac could fail to establish a channel, so this pins both to one answer.
///
/// Set UNPEEL_EMIT_RELAY_KAT=1 to print the current vectors as JSON (used to
/// refresh the server repo's protocol/relay-kat-vectors-v1.json after an
/// intentional change; the JS relay KAT and the Rust Host KAT read that file).
final class RelayCryptoVectorTests: XCTestCase {
    // Fixed inputs (no RNG) so the outputs are reproducible cross-language.
    // `truncatingIfNeeded` mirrors the JS side's `& 0xff` byte wrapping so the
    // two implementations feed the crypto identical bytes.
    private let e2eKey = Data((0 ..< 32).map { UInt8(truncatingIfNeeded: $0) })
    private let sharedSecret = Data((0 ..< 32).map { UInt8(truncatingIfNeeded: 0x40 + $0) })
    private let clientSalt = Data((0 ..< 16).map { UInt8(truncatingIfNeeded: 0x10 + $0) })
    private let hostSalt = Data((0 ..< 16).map { UInt8(truncatingIfNeeded: 0xA0 + $0) })
    private let deviceID = "phone-kat-1"
    private let clientEph = Data((0 ..< 32).map { UInt8(truncatingIfNeeded: 0x80 + $0) })
    private let hostEph = Data((0 ..< 32).map { UInt8(truncatingIfNeeded: 0xC0 + $0 * 3) })

    func testTranscriptMACKnownAnswer() {
        let mac = RelayHandshake.transcriptMAC(
            e2eKey: e2eKey,
            deviceID: deviceID,
            clientSalt: clientSalt,
            hostSalt: hostSalt,
            clientEphemeralPublicKey: clientEph,
            hostEphemeralPublicKey: hostEph
        )
        // Locked value — the JS KAT must reproduce it. Deterministic from the
        // fixed inputs above; regenerate with UNPEEL_EMIT_RELAY_KAT=1 only on
        // an intentional protocol change.
        XCTAssertEqual(
            mac.base64EncodedString(),
            Self.expected.transcriptMAC,
            "transcript MAC drifted — Swift and JS handshakes would disagree"
        )
    }

    func testSealKnownAnswerOpensInBothImplementations() throws {
        // A client-direction session with the fixed shared secret.
        var client = try RelayCryptoSession(
            e2eKey: e2eKey,
            sharedSecret: sharedSecret,
            clientSalt: clientSalt,
            hostSalt: hostSalt,
            isHost: false
        )
        let sealed = try client.seal(Data("known-answer-plaintext".utf8))

        // The counter-prefixed layout is deterministic; the GCM ciphertext is
        // deterministic too (fixed key + counter nonce). Lock the whole frame.
        XCTAssertEqual(
            sealed.base64EncodedString(),
            Self.expected.sealedFrame,
            "sealed frame drifted — Swift and JS AEAD would disagree"
        )

        // And it must open on the matching host session.
        var host = try RelayCryptoSession(
            e2eKey: e2eKey,
            sharedSecret: sharedSecret,
            clientSalt: clientSalt,
            hostSalt: hostSalt,
            isHost: true
        )
        XCTAssertEqual(try host.open(sealed), Data("known-answer-plaintext".utf8))
    }

    /// Prints the current vectors so the locked constants + the JS KAT JSON
    /// can be refreshed after an intentional protocol change.
    func testEmitVectorsWhenRequested() throws {
        guard ProcessInfo.processInfo.environment["UNPEEL_EMIT_RELAY_KAT"] == "1" else {
            throw XCTSkip("set UNPEEL_EMIT_RELAY_KAT=1 to emit vectors")
        }
        let mac = RelayHandshake.transcriptMAC(
            e2eKey: e2eKey, deviceID: deviceID,
            clientSalt: clientSalt, hostSalt: hostSalt,
            clientEphemeralPublicKey: clientEph, hostEphemeralPublicKey: hostEph
        )
        var client = try RelayCryptoSession(
            e2eKey: e2eKey, sharedSecret: sharedSecret,
            clientSalt: clientSalt, hostSalt: hostSalt, isHost: false
        )
        let sealed = try client.seal(Data("known-answer-plaintext".utf8))
        print("RELAY_KAT_JSON={\"transcriptMAC\":\"\(mac.base64EncodedString())\",\"sealedFrame\":\"\(sealed.base64EncodedString())\"}")
    }

    // Locked expected outputs (base64). These exact strings are also committed
    // to protocol/relay-kat-vectors-v1.json in the server repo (shipped inside
    // every CLI archive as protocol/), which the WebCrypto relay KAT and the
    // Rust Host KAT reproduce independently — so a match proves Swift, JS, and
    // Rust agree byte for byte. After the Apple repo split, refresh the locked
    // strings from the pinned archive's copy. Regenerate with
    // UNPEEL_EMIT_RELAY_KAT=1 on an intentional change.
    private enum expected {
        static let transcriptMAC = "+BBTo0DBUwkP829M9w6eviupf+3pv5XxzrtNnUeYNQc="
        static let sealedFrame = "AAAAAAAAAAFXoFTergM+a27Rbw/LTzDUy/OhPJRbGDcDIEpfVPJbKdy1zzcoCQ=="
    }
}
