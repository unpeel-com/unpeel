//
//  RemoteTerminalWebSocket.swift
//  UnpeelIOS
//
//  The live side of the terminal-output WebSocket: certificate-pinned
//  URLSession, the connection wrapper the renderer's stream loop drives, the
//  input router that prefers a healthy WS over the HTTP write, and the
//  discovery observable that carries the latest advertised port/fingerprint
//  from bootstrap polls to the renderer.
//
//  Pure protocol parsing/selection logic lives in
//  RemoteTerminalStreamTransport.swift (unit-tested, socket-free).
//

import CryptoKit
import Foundation
import Security

/// Latest advertised `unpeel-host __remote__` endpoint for the connected
/// Mac. `RemotePreviewStore` refreshes it from every bootstrap poll (the
/// port is OS-assigned per server run, so the renderer must read the
/// freshest value before every (re)connect attempt); pairing seeds it and
/// unpairing clears it. Nil means "no WS available" — dev bridge, server
/// down, or a pre-WS Mac build — and the stream loop stays on HTTP.
@MainActor
final class RemoteServerDiscovery: ObservableObject {
    static let shared = RemoteServerDiscovery()

    @Published private(set) var endpoint: RemoteServerEndpoint?

    func update(port: Int?, certificateFingerprint: String?) {
        let next = RemoteTerminalTransportSelector.endpoint(
            port: port,
            fingerprint: certificateFingerprint
        )
        if endpoint != next {
            endpoint = next
        }
    }

    func clear() {
        if endpoint != nil {
            endpoint = nil
        }
    }
}

enum RemoteTerminalWebSocketError: Error {
    case helloTimeout
    case protocolViolation
    case closed
}

/// Accepts the remote server's self-signed TLS handshake iff the SHA-256 of
/// the leaf certificate's DER equals the fingerprint the Mac advertised over
/// the already-paired mobile channel. Exact pin, no bypass: a mismatch (or
/// an unreadable chain) cancels the handshake.
final class RemoteCertificatePinningDelegate: NSObject, URLSessionDelegate, @unchecked Sendable {
    private let pinnedFingerprint: String

    init(pinnedFingerprint: String) {
        self.pinnedFingerprint = pinnedFingerprint.lowercased()
    }

    func urlSession(
        _ session: URLSession,
        didReceive challenge: URLAuthenticationChallenge
    ) async -> (URLSession.AuthChallengeDisposition, URLCredential?) {
        guard challenge.protectionSpace.authenticationMethod == NSURLAuthenticationMethodServerTrust,
              let trust = challenge.protectionSpace.serverTrust
        else {
            return (.performDefaultHandling, nil)
        }
        guard let leaf = Self.leafCertificateFingerprint(of: trust),
              leaf == pinnedFingerprint
        else {
            return (.cancelAuthenticationChallenge, nil)
        }
        return (.useCredential, URLCredential(trust: trust))
    }

    /// Lowercase hex SHA-256 of the leaf certificate DER.
    static func leafCertificateFingerprint(of trust: SecTrust) -> String? {
        guard let chain = SecTrustCopyCertificateChain(trust) as? [SecCertificate],
              let leaf = chain.first
        else { return nil }
        let der = SecCertificateCopyData(leaf) as Data
        return SHA256.hash(data: der).map { String(format: "%02x", $0) }.joined()
    }
}

/// One output-stream WebSocket connection. Owns its pinned URLSession and
/// task; thread-safe (`@unchecked Sendable`, mutable state behind a lock)
/// because the renderer's main-actor loop receives while the write queue's
/// drain sends from arbitrary executors.
final class RemoteTerminalWebSocketConnection: @unchecked Sendable {
    private let session: URLSession
    private let task: URLSessionWebSocketTask
    private let lock = NSLock()
    private var closed = false
    private var pingTask: Task<Void, Never>?

    init(url: URL, pinnedFingerprint: String) {
        let configuration = URLSessionConfiguration.ephemeral
        configuration.waitsForConnectivity = false
        // Generous request timer: steady-state liveness is owned by the ping
        // watchdog below (server pings every 20s keep healthy idle streams
        // alive); this only bounds a wedged handshake.
        configuration.timeoutIntervalForRequest = 120
        session = URLSession(
            configuration: configuration,
            delegate: RemoteCertificatePinningDelegate(pinnedFingerprint: pinnedFingerprint),
            delegateQueue: nil
        )
        task = session.webSocketTask(with: url)
        // Replay bursts can exceed the 1MB default.
        task.maximumMessageSize = 16 * 1024 * 1024
        task.resume()
        startPingWatchdog()
    }

    var isClosed: Bool {
        lock.lock()
        defer { lock.unlock() }
        return closed
    }

    /// Cancellation-aware receive: cancelling the surrounding task (renderer
    /// stop, session switch, cache eviction) cancels the socket, which
    /// unblocks this await with an error.
    func receive() async throws -> URLSessionWebSocketTask.Message {
        try await withTaskCancellationHandler {
            try await task.receive()
        } onCancel: {
            self.abort()
        }
    }

    /// The server's first frame must be the hello text frame; anything else
    /// (or silence) means an incompatible/unhealthy server — the caller
    /// falls back to HTTP.
    func receiveHello(timeout: Duration = .seconds(10)) async throws -> RemoteTerminalWSHello {
        try await withThrowingTaskGroup(of: RemoteTerminalWSHello.self) { group in
            group.addTask {
                let message = try await self.receive()
                guard case .string(let text) = message,
                      case .hello(let hello) = RemoteTerminalWSServerMessage.parse(text: text)
                else { throw RemoteTerminalWebSocketError.protocolViolation }
                return hello
            }
            group.addTask {
                try await Task.sleep(for: timeout)
                throw RemoteTerminalWebSocketError.helloTimeout
            }
            guard let hello = try await group.next() else {
                throw RemoteTerminalWebSocketError.closed
            }
            group.cancelAll()
            return hello
        }
    }

    /// Raw PTY input as ordered JSON text frames (no ack — the echo arrives
    /// via the output stream).
    func sendInput(_ text: String, writeID: String? = nil) async throws {
        guard !isClosed else { throw RemoteTerminalWebSocketError.closed }
        for frame in RemoteTerminalWSClientMessage.inputFrames(for: text, writeID: writeID) {
            try await task.send(.string(frame))
        }
    }

    func close() {
        pingTask?.cancel()
        markClosed()
        task.cancel(with: .normalClosure, reason: nil)
        session.finishTasksAndInvalidate()
    }

    deinit {
        pingTask?.cancel()
        task.cancel(with: .goingAway, reason: nil)
        session.finishTasksAndInvalidate()
    }

    private func markClosed() {
        lock.lock()
        closed = true
        lock.unlock()
    }

    private func abort() {
        markClosed()
        task.cancel(with: .goingAway, reason: nil)
    }

    /// Detects a silently dead Mac (sleep, Wi-Fi drop): a failed pong
    /// aborts the socket so the receive loop unblocks and the stream falls
    /// back to HTTP instead of hanging on a black-holed TCP connection.
    private func startPingWatchdog() {
        pingTask = Task { [weak self] in
            while !Task.isCancelled {
                try? await Task.sleep(for: .seconds(30))
                guard let self, !self.isClosed, !Task.isCancelled else { return }
                self.task.sendPing { error in
                    if error != nil {
                        self.abort()
                    }
                }
            }
        }
    }
}

/// Routes the renderer's ordered input queue through the active transport:
/// the WS text frame while a connection is healthy, the proven HTTP write
/// otherwise. A WS send failure still lands the bytes over HTTP — failed
/// keystrokes are input the Mac never saw.
final class RemoteTerminalInputRouter: @unchecked Sendable {
    private let lock = NSLock()
    private var webSocket: RemoteTerminalWebSocketConnection?
    private let httpSend: @Sendable (String, String) async throws -> Void

    /// `httpSend` receives the input text and the idempotency key for the same
    /// logical send that was first attempted over the WebSocket.
    init(httpSend: @escaping @Sendable (String, String) async throws -> Void) {
        self.httpSend = httpSend
    }

    func adoptWebSocket(_ connection: RemoteTerminalWebSocketConnection) {
        lock.lock()
        webSocket = connection
        lock.unlock()
    }

    /// Clears only if `connection` is still the active one — a newer attempt
    /// may have installed itself before the old attempt's defer ran.
    func retireWebSocket(_ connection: RemoteTerminalWebSocketConnection) {
        lock.lock()
        if webSocket === connection {
            webSocket = nil
        }
        lock.unlock()
    }

    private var healthyWebSocket: RemoteTerminalWebSocketConnection? {
        lock.lock()
        defer { lock.unlock() }
        guard let webSocket, !webSocket.isClosed else { return nil }
        return webSocket
    }

    func send(_ text: String) async throws {
        // One idempotency key per logical send, shared by both transports. If
        // the WebSocket attempt's delivery is ambiguous (it timed out after
        // the bytes may already have reached the Host) the HTTP fallback below
        // reuses this key so the Host applies the keystroke once, not twice.
        let writeID = UUID().uuidString
        // One idempotency key names one complete byte string. A split WS send
        // could time out after only some frames arrived, which cannot be made
        // equivalent to retrying the whole string under one key. Route large
        // pastes directly over HTTP so delivery is one idempotent operation.
        if text.utf8.count > RemoteTerminalWSClientMessage.maxInputBytesPerFrame {
            try await httpSend(text, writeID)
            return
        }
        if let webSocket = healthyWebSocket {
            do {
                // A WS reported "not closed" can still be half-dead (network
                // blip, reconnect churn): sendInput then blocks on the dying
                // socket with no timeout, and since the write queue is serial
                // every queued keystroke waits behind it — the 3-5s typing
                // lag. Bound the WS attempt; on stall, retire the socket so
                // later keystrokes skip it, and fall through to HTTP so this
                // one still lands promptly.
                try await Self.withTimeout(seconds: 0.5) {
                    try await webSocket.sendInput(text, writeID: writeID)
                }
                return
            } catch {
                retireWebSocket(webSocket)
            }
        }
        try await httpSend(text, writeID)
    }

    private static func withTimeout(
        seconds: TimeInterval,
        _ operation: @escaping @Sendable () async throws -> Void
    ) async throws {
        try await withThrowingTaskGroup(of: Void.self) { group in
            group.addTask { try await operation() }
            group.addTask {
                try await Task.sleep(nanoseconds: UInt64(seconds * 1_000_000_000))
                throw RemoteTerminalInputTimeout()
            }
            // First to finish wins; cancel the loser (the sleep, or the
            // stalled send — URLSessionWebSocketTask.send honors cancellation).
            defer { group.cancelAll() }
            try await group.next()
        }
    }
}

private struct RemoteTerminalInputTimeout: Error {}
