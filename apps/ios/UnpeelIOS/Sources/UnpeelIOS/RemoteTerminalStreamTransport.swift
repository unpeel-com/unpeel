//
//  RemoteTerminalStreamTransport.swift
//  UnpeelIOS
//
//  Pure protocol/selection logic for the terminal output WebSocket exposed
//  by the Mac's `unpeel-host __remote__` server: hello/error frame decoding,
//  binary frame parsing (8-byte big-endian output.bin offset prefix),
//  certificate fingerprint normalization, and the transport-selection
//  decision (WS candidate vs. the HTTP long-poll fallback).
//
//  Deliberately socket-free and side-effect-free so every piece is unit
//  testable; the live connection lives in RemoteTerminalWebSocket.swift.
//

import Foundation

/// The advertised `unpeel-host __remote__` endpoint, as discovered from the
/// latest bootstrap/pairing response. The port is OS-assigned per server run
/// (never cache it across reconnects — always read the freshest value); the
/// fingerprint is stable across restarts and already normalized.
struct RemoteServerEndpoint: Equatable, Sendable {
    let port: Int
    /// Normalized lowercase hex SHA-256 of the TLS leaf certificate DER.
    let certificateFingerprint: String
}

/// Everything needed for one WS connect attempt: host from the paired mobile
/// endpoint, port + pin from discovery, the phone's paired bearer token.
struct RemoteTerminalWebSocketCandidate: Equatable, Sendable {
    let host: String
    let port: Int
    let certificateFingerprint: String
    let token: String
}

enum RemoteTerminalTransportSelector {
    /// Strict-but-liberal fingerprint normalization: strips an optional
    /// `sha256:` prefix, colons, and whitespace, lowercases, and requires
    /// exactly 64 hex characters. Anything else is unusable for pinning —
    /// and no pin means no WS (there is deliberately no bypass).
    static func normalizedFingerprint(_ raw: String?) -> String? {
        guard let raw else { return nil }
        var value = raw.trimmingCharacters(in: .whitespacesAndNewlines).lowercased()
        if value.hasPrefix("sha256:") {
            value = String(value.dropFirst("sha256:".count))
        }
        value = value.replacingOccurrences(of: ":", with: "")
        guard value.count == 64, value.allSatisfy({ Self.hexDigits.contains($0) }) else {
            return nil
        }
        return value
    }

    static func fingerprintsMatch(_ a: String?, _ b: String?) -> Bool {
        guard let a = normalizedFingerprint(a), let b = normalizedFingerprint(b) else {
            return false
        }
        return a == b
    }

    /// Discovery: a usable remote-server endpoint requires both a live port
    /// and a valid fingerprint. The dev bridge advertises neither, so it
    /// always resolves nil — the HTTP long-poll stays its only transport.
    static func endpoint(port: Int?, fingerprint: String?) -> RemoteServerEndpoint? {
        guard let port, (1...65_535).contains(port),
              let normalized = normalizedFingerprint(fingerprint)
        else { return nil }
        return RemoteServerEndpoint(port: port, certificateFingerprint: normalized)
    }

    /// The transport decision for one stream attempt: WS when the Mac
    /// advertises the remote server AND we hold a paired token to present;
    /// nil means HTTP long-poll (dev bridge, server down, or pre-WS build).
    static func candidate(
        endpoint: RemoteServerEndpoint?,
        baseURL: URL,
        authToken: String?
    ) -> RemoteTerminalWebSocketCandidate? {
        guard let endpoint,
              let host = baseURL.host(percentEncoded: false), !host.isEmpty,
              let token = authToken, !token.isEmpty
        else { return nil }
        return RemoteTerminalWebSocketCandidate(
            host: host,
            port: endpoint.port,
            certificateFingerprint: endpoint.certificateFingerprint,
            token: token
        )
    }

    /// `wss://<host>:<port>/api/sessions/<id>/output?token=...[&offset=N]`
    static func webSocketOutputURL(
        host: String,
        port: Int,
        sessionID: String,
        token: String,
        offset: UInt64?
    ) -> URL? {
        guard !host.isEmpty, !sessionID.isEmpty else { return nil }
        var components = URLComponents()
        components.scheme = "wss"
        components.host = host
        components.port = port
        components.path = "/api/sessions/\(sessionID)/output"
        var items = [URLQueryItem(name: "token", value: token)]
        if let offset {
            items.append(URLQueryItem(name: "offset", value: "\(offset)"))
        }
        components.queryItems = items
        return components.url
    }

    private static let hexDigits = Set("0123456789abcdef")
}

/// The server's first text frame on a successful upgrade.
struct RemoteTerminalWSHello: Equatable, Sendable, Decodable {
    let protocolVersion: Int
    let sessionID: String
    let state: String
    /// Total output.bin size at connect time.
    let outputSize: UInt64
    /// The offset the client asked for (nil on a fresh connect).
    let requestedOffset: UInt64?
    /// Where the binary stream actually begins.
    let startOffset: UInt64
    /// True when the requested offset was unusable (beyond the file or too
    /// far behind the tail) and the server restarted from an aligned tail —
    /// the client must clear before feeding, like an HTTP rebase.
    let rebased: Bool
    let cols: Int?
    let rows: Int?
    /// DEC-mode restore preamble (base64) the client feeds into its freshly
    /// reset VT before the replayed tail — the mouse-tracking / alt-screen
    /// sequences that scrolled out of the retained journal. Absent from
    /// older Hosts and at the session origin. Not journal bytes: it never
    /// moves `startOffset` or the resume cursor.
    let modePreambleBase64: String?

    var modePreamble: Data? {
        guard let encoded = modePreambleBase64,
              let bytes = Data(base64Encoded: encoded),
              !bytes.isEmpty
        else { return nil }
        return bytes
    }

    enum CodingKeys: String, CodingKey {
        case protocolVersion = "protocol"
        case sessionID = "session_id"
        case state
        case outputSize = "output_size"
        case requestedOffset = "requested_offset"
        case startOffset = "start_offset"
        case rebased
        case cols
        case rows
        case modePreambleBase64 = "mode_preamble_base64"
    }
}

/// Server→client text frames: the hello, or non-fatal in-stream errors.
enum RemoteTerminalWSServerMessage: Equatable, Sendable {
    case hello(RemoteTerminalWSHello)
    case error(String)
    case unknown

    static func parse(text: String) -> RemoteTerminalWSServerMessage {
        let data = Data(text.utf8)
        guard let object = (try? JSONSerialization.jsonObject(with: data)) as? [String: Any],
              let type = object["type"] as? String
        else { return .unknown }
        switch type {
        case "hello":
            guard let hello = try? JSONDecoder().decode(RemoteTerminalWSHello.self, from: data) else {
                return .unknown
            }
            return .hello(hello)
        case "error":
            return .error((object["message"] as? String) ?? "unknown error")
        default:
            return .unknown
        }
    }
}

/// Server→client binary frame: bytes 0-7 are the big-endian u64 output.bin
/// offset of the first payload byte, the rest is raw terminal bytes (no
/// base64). `offset + payload.count` is the resume offset — the same offset
/// space the HTTP long-poll uses, so the two transports are interchangeable.
struct RemoteTerminalWSBinaryFrame: Equatable, Sendable {
    let offset: UInt64
    let payload: Data

    static func parse(_ data: Data) -> RemoteTerminalWSBinaryFrame? {
        guard data.count >= 8 else { return nil }
        // Relative indexing: `data` may be a slice with a non-zero base.
        var offset: UInt64 = 0
        for byte in data.prefix(8) {
            offset = (offset << 8) | UInt64(byte)
        }
        return RemoteTerminalWSBinaryFrame(offset: offset, payload: data.dropFirst(8))
    }
}

/// Client→server JSON text frames.
enum RemoteTerminalWSClientMessage {
    /// The server caps one input message's data at 64KB; chunk well under it
    /// (on character boundaries, so escape sequences and multi-byte UTF-8
    /// never split mid-scalar — a String iterates whole characters).
    static let maxInputBytesPerFrame = 32 * 1024

    /// One raw-PTY input as one or more `{"type":"input","data":...}` frames,
    /// in order. No ack is expected — the echo arrives via output.
    ///
    /// `writeID` is the idempotency key the caller also sends on the HTTP
    /// fallback for this same logical send. It is attached only when the send
    /// fits in one frame. The live input router sends larger values directly
    /// over HTTP, avoiding an ambiguous partial multi-frame WS delivery.
    static func inputFrames(
        for text: String,
        writeID: String? = nil,
        maxBytes: Int = maxInputBytesPerFrame
    ) -> [String] {
        guard !text.isEmpty else { return [] }
        if text.utf8.count <= maxBytes {
            return [encodeInput(text, writeID: writeID)]
        }
        var frames: [String] = []
        var chunk = ""
        var chunkBytes = 0
        for character in text {
            let size = character.utf8.count
            if chunkBytes + size > maxBytes, !chunk.isEmpty {
                frames.append(encodeInput(chunk, writeID: nil))
                chunk = ""
                chunkBytes = 0
            }
            chunk.append(character)
            chunkBytes += size
        }
        if !chunk.isEmpty {
            frames.append(encodeInput(chunk, writeID: nil))
        }
        return frames
    }

    private struct InputPayload: Encodable {
        let type: String
        let data: String
        let wid: String?
    }

    private static func encodeInput(_ data: String, writeID: String?) -> String {
        let payload = InputPayload(
            type: "input",
            data: data,
            wid: (writeID?.isEmpty == false) ? writeID : nil
        )
        let encoded = (try? JSONEncoder().encode(payload)) ?? Data()
        return String(decoding: encoded, as: UTF8.self)
    }
}
