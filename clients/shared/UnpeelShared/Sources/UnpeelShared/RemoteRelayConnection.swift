//
//  RemoteRelayConnection.swift
//  UnpeelShared
//
//  The Controller side of Unpeel Remote: one WebSocket to the Cloudflare relay
//  carrying end-to-end encrypted `/mobile/*` request/response frames
//  (`RelayProtocol.swift`). The relay authenticates the
//  socket with the per-device relayToken; the content is AES-GCM sealed
//  with the per-device e2eKey, so the relay — and unpeel.com — can read
//  none of it. Lazily connected; any failure tears the connection down and
//  the next request reconnects with a fresh handshake. This implementation
//  is shared verbatim by iPhone/iPad and macOS Controllers.
//

import Foundation
import Security

/// Whether a failed Relay request is proven not to have entered the encrypted
/// channel, or may have reached the Host without a correlated response.
public enum RemoteRelayDeliveryState: String, Sendable {
    case notSent
    case outcomeUnknown
}

/// Transport-level failures exposed to semantic Controller backends. Keeping
/// delivery certainty here lets the shared Rust backend preserve at-most-once
/// effects without reimplementing the shipped Relay connection or crypto.
public enum RemoteRelayConnectionError: Error, LocalizedError, Sendable {
    case generationChanged
    case transport(delivery: RemoteRelayDeliveryState, message: String)
    case timedOut(delivery: RemoteRelayDeliveryState)

    public var errorDescription: String? {
        switch self {
        case .generationChanged:
            "The Link connection changed before this request was sent."
        case let .transport(_, message):
            message
        case .timedOut:
            "The Link request timed out."
        }
    }
}

/// A response plus the exact E2E/WebSocket generation that carried it. Reads
/// may open a fresh generation; effects bind to the generation whose bootstrap
/// was accepted and therefore fail before send if that connection changed.
public struct RemoteRelayTransportResponse: Sendable {
    public let response: RelayTunnelResponse
    public let connectionGeneration: UInt64

    public init(response: RelayTunnelResponse, connectionGeneration: UInt64) {
        self.response = response
        self.connectionGeneration = connectionGeneration
    }
}

/// When a relay request that missed its deadline should also retire the
/// socket it rode. Pure so the rule is testable without a relay.
public enum RemoteRelayRequestExpiryPolicy {
    /// Longest a live socket can go without inbound traffic: the keepalive
    /// ping cadence (15 s) plus round-trip slack. Any pong or frame within
    /// this window proves the path, whatever one Host request is doing.
    public static let silenceLimit: TimeInterval = 20

    public static func shouldRetireConnection(
        sentAt: Date,
        lastIncomingAt: Date,
        now: Date
    ) -> Bool {
        // Inbound traffic after the send proves the transport outright.
        guard lastIncomingAt < sentAt else { return false }
        return now.timeIntervalSince(lastIncomingAt) > silenceLimit
    }
}

public actor RemoteRelayConnection {
    private struct EstablishedConnection: @unchecked Sendable {
        let socket: URLSessionWebSocketTask
        let crypto: RelayCryptoSession
    }

    private let credentials: RelayCredentials
    private let deviceID: String

    private var socket: URLSessionWebSocketTask?
    private var crypto: RelayCryptoSession?
    private var receiveTask: Task<Void, Never>?
    private var pingTask: Task<Void, Never>?
    private var connectionTask: Task<EstablishedConnection, Error>?
    private var connectionGeneration: UInt64 = 0
    private var nextRequestID: UInt64 = 0
    private var pending: [UInt64: CheckedContinuation<RelayTunnelResponse, Error>] = [:]
    /// When the current socket last proved the transport alive (any inbound
    /// frame or an answered keepalive pong). A connection can look installed
    /// while its TCP path is black-holed — the network changed, the device
    /// slept, a NAT mapping expired — and `receive()` then blocks forever
    /// without erroring, so liveness must be tracked explicitly.
    private var lastIncomingAt = Date.distantPast
    /// Consumer of Host-pushed output frames (one at a time — a Controller
    /// views one Session). Finished on teardown so the consumer's loop ends.
    private var pushContinuation: AsyncStream<RelayStreamPush>.Continuation?

    /// Register (replacing any previous) the consumer for pushed output
    /// frames. Frames for sessions the consumer no longer cares about are
    /// its to ignore.
    public func outputPushFrames() -> AsyncStream<RelayStreamPush> {
        pushContinuation?.finish()
        let (stream, continuation) = AsyncStream.makeStream(of: RelayStreamPush.self)
        pushContinuation = continuation
        return stream
    }

    public init(credentials: RelayCredentials, deviceID: String) {
        self.credentials = credentials
        self.deviceID = deviceID
    }

    deinit {
        socket?.cancel(with: .goingAway, reason: nil)
        receiveTask?.cancel()
        pingTask?.cancel()
    }

    // MARK: - Requests

    public func perform(
        method: String,
        path: String,
        query: [String: String],
        auth: String?,
        contentType: String?,
        body: Data?,
        timeout: TimeInterval
    ) async throws -> RelayTunnelResponse {
        nextRequestID += 1
        let request = RelayTunnelRequest(
            id: nextRequestID,
            method: method,
            path: path,
            query: query,
            auth: auth,
            contentType: contentType,
            body: body
        )
        return try await perform(
            request: request,
            requiredConnectionGeneration: nil,
            timeout: timeout
        ).response
    }

    /// Perform an already-numbered Host request. A nil generation may open a
    /// fresh connection (bootstrap/read recovery). A non-nil generation is
    /// fail-closed: it never reconnects and never sends on a successor socket.
    public func perform(
        request: RelayTunnelRequest,
        requiredConnectionGeneration: UInt64?,
        timeout: TimeInterval
    ) async throws -> RemoteRelayTransportResponse {
        nextRequestID = max(nextRequestID, request.id)
        // Measure the complete JSON/base64 envelope before connecting or
        // sealing. An oversized local request must not burn a crypto counter
        // or tear down an otherwise healthy relay connection.
        let plaintext: Data
        do {
            plaintext = try RelayTunnelCodec.encodeRequest(request)
        } catch {
            throw RemoteRelayConnectionError.transport(
                delivery: .notSent,
                message: error.localizedDescription
            )
        }

        if let requiredConnectionGeneration {
            guard socket != nil,
                  crypto != nil,
                  connectionGeneration == requiredConnectionGeneration
            else {
                throw RemoteRelayConnectionError.generationChanged
            }
        } else {
            do {
                try await ensureConnected()
            } catch {
                throw RemoteRelayConnectionError.transport(
                    delivery: .notSent,
                    message: error.localizedDescription
                )
            }
        }
        guard var crypto else {
            throw RemoteRelayConnectionError.generationChanged
        }
        let sealed: Data
        do {
            sealed = try crypto.seal(plaintext)
        } catch {
            teardown(error: URLError(.cannotConnectToHost))
            throw RemoteRelayConnectionError.transport(
                delivery: .notSent,
                message: error.localizedDescription
            )
        }
        self.crypto = crypto
        // Client sockets carry bare opaque bytes — the DO wraps them with
        // this connection's id before piping to the Host (and strips the
        // wrapper on the way back).
        let frame = sealed
        guard let socket else {
            throw RemoteRelayConnectionError.generationChanged
        }
        let generation = connectionGeneration
        let sentAt = Date()

        let response = try await withCheckedThrowingContinuation { continuation in
            pending[request.id] = continuation
            pendingSentAt[request.id] = sentAt
            socket.send(.data(frame)) { [weak self] error in
                guard let error else { return }
                Task { [weak self] in
                    await self?.teardownIfCurrent(socket, generation: generation, error: error)
                }
            }
            Task { [weak self] in
                try? await Task.sleep(nanoseconds: Self.timeoutNanoseconds(timeout))
                await self?.expireRequest(
                    id: request.id,
                    socket: socket,
                    generation: generation,
                    sentAt: sentAt
                )
            }
        }
        return RemoteRelayTransportResponse(
            response: response,
            connectionGeneration: generation
        )
    }

    /// Close the current socket and fail every in-flight call. The same actor
    /// may reconnect later for an unconstrained bootstrap.
    public func close() {
        teardown(error: URLError(.cancelled))
    }

    private func settle(id: UInt64, with result: Result<RelayTunnelResponse, Error>) {
        guard let continuation = pending.removeValue(forKey: id) else { return }
        if let sentAt = pendingSentAt.removeValue(forKey: id), case .success = result {
            lastMeasuredRoundTrip = Date().timeIntervalSince(sentAt)
        }
        continuation.resume(with: result)
    }

    /// A request that missed its deadline fails on its own. The connection is
    /// retired only when the socket has ALSO been silent past the keepalive
    /// cadence (`RemoteRelayRequestExpiryPolicy`): pongs answer every 15 s on
    /// a live path, so silence that long is a black-holed socket, while a
    /// slow Host behind a live socket is just a slow request. Tearing down on
    /// every miss used to convert one late bootstrap on cellular into a
    /// reconnect, a re-subscribe burst, and the next late bootstrap.
    private func expireRequest(
        id: UInt64,
        socket task: URLSessionWebSocketTask,
        generation: UInt64,
        sentAt: Date
    ) {
        guard pending[id] != nil else { return }
        settle(
            id: id,
            with: .failure(RemoteRelayConnectionError.timedOut(delivery: .outcomeUnknown))
        )
        guard generation == connectionGeneration, task === socket else { return }
        if RemoteRelayRequestExpiryPolicy.shouldRetireConnection(
            sentAt: sentAt,
            lastIncomingAt: lastIncomingAt,
            now: Date()
        ) {
            teardown(error: URLError(.timedOut))
        }
    }

    /// Wall-clock duration of the most recent answered request on this
    /// connection. Callers scale deadlines for slow paths (cellular Link)
    /// from it; nil until the first response.
    public private(set) var lastMeasuredRoundTrip: TimeInterval?
    private var pendingSentAt: [UInt64: Date] = [:]

    private func teardown(error: Error) {
        connectionGeneration &+= 1
        connectionTask?.cancel()
        connectionTask = nil
        socket?.cancel(with: .goingAway, reason: nil)
        socket = nil
        crypto = nil
        receiveTask?.cancel()
        receiveTask = nil
        pingTask?.cancel()
        pingTask = nil
        let waiting = pending
        pending.removeAll()
        pendingSentAt.removeAll()
        let pendingError = RemoteRelayConnectionError.transport(
            delivery: .outcomeUnknown,
            message: error.localizedDescription
        )
        for continuation in waiting.values {
            continuation.resume(throwing: pendingError)
        }
        // End the push-frame consumer so its loop exits and re-subscribes
        // (or falls back) through a fresh connection.
        pushContinuation?.finish()
        pushContinuation = nil
    }

    // MARK: - Connection + handshake

    private func ensureConnected() async throws {
        if socket != nil, crypto != nil { return }
        if let connectionTask {
            let generation = connectionGeneration
            let established = try await connectionTask.value
            try installIfCurrent(established, generation: generation)
            guard socket != nil, crypto != nil else { throw URLError(.cannotConnectToHost) }
            return
        }
        // There is no live transport here: a real socket loss already ran
        // `teardown`, and a failed establishment never installed one. Starting
        // the lazy/replacement connection is therefore not itself a teardown.
        // In particular, preserve an output-push consumer registered just
        // before its subscribe request; finishing it here made resubscribe
        // after a dropped Link connection silently produce an empty stream.
        connectionGeneration &+= 1
        let generation = connectionGeneration
        let credentials = self.credentials
        let deviceID = self.deviceID
        let task = Task<EstablishedConnection, Error> {
            try await Self.establish(credentials: credentials, deviceID: deviceID)
        }
        connectionTask = task
        do {
            let established = try await task.value
            try installIfCurrent(established, generation: generation)
        } catch {
            if connectionGeneration == generation {
                connectionTask = nil
                socket = nil
                crypto = nil
            }
            throw error
        }
    }

    private func installIfCurrent(
        _ established: EstablishedConnection,
        generation: UInt64
    ) throws {
        guard connectionGeneration == generation else {
            established.socket.cancel(with: .goingAway, reason: nil)
            throw URLError(.cancelled)
        }
        guard socket == nil || crypto == nil else { return }
        connectionTask = nil
        socket = established.socket
        crypto = established.crypto
        // The handshake just exchanged frames — that is this connection's
        // first proof of life.
        lastIncomingAt = Date()
        startReceiveLoop(socket: established.socket, generation: generation)
        startPingLoop(socket: established.socket, generation: generation)
    }

    private static func establish(
        credentials: RelayCredentials,
        deviceID: String
    ) async throws -> EstablishedConnection {
        guard let e2eKey = credentials.e2eKey, e2eKey.count == 32 else {
            throw URLError(.userAuthenticationRequired)
        }
        let base = credentials.relayURL.absoluteString
            .trimmingCharacters(in: CharacterSet(charactersIn: "/"))
        guard let url = URL(string: "\(base)/v1/client/\(credentials.macID)") else {
            throw URLError(.badURL)
        }

        var request = URLRequest(url: url)
        request.timeoutInterval = 10
        // The relayToken rides a WS subprotocol header, never the URL query,
        // so it can't leak into relay/proxy access logs. A stable protocol
        // is offered alongside it for the server to echo in its 101.
        request.setValue(
            "unpeel-relay, unpeel-relay-token.\(credentials.relayToken)",
            forHTTPHeaderField: "Sec-WebSocket-Protocol"
        )
        let task = URLSession.shared.webSocketTask(with: request)
        task.maximumMessageSize = RelayProtocol.maxFrameBytes + 64
        task.resume()

        // Forward-secret handshake: plaintext salts + ephemeral X25519 public
        // keys both ways (worthless to the relay without the device key),
        // then the host proves it holds the device key via a transcript MAC.
        // After that, everything is sealed under keys bound to the ephemeral
        // secret — so a later static-key leak can't decrypt this session.
        let clientSalt = try Self.randomBytes(16)
        let clientEphemeral = RelayHandshake.EphemeralKeyPair()
        let hello = try JSONEncoder().encode(RelayClientHello(
            deviceID: deviceID,
            salt: clientSalt,
            ephemeralPublicKey: clientEphemeral.publicKey
        ))
        do {
            try await task.send(.data(hello))
        } catch {
            task.cancel(with: .goingAway, reason: nil)
            throw error
        }
        let reply: Data
        do {
            reply = try await Self.receiveData(from: task, timeout: 10)
        } catch {
            task.cancel(with: .goingAway, reason: nil)
            throw error
        }
        guard let hostHello = try? JSONDecoder().decode(RelayHostHello.self, from: reply),
              hostHello.v == RelayProtocol.version,
              let hostSalt = hostHello.salt, hostSalt.count == 16,
              let hostEphemeral = hostHello.ephemeralPublicKey,
              let hostMAC = hostHello.mac
        else {
            task.cancel(with: .protocolError, reason: nil)
            throw URLError(.cannotConnectToHost)
        }
        // Verify the host's transcript MAC BEFORE deriving/using any key:
        // proves the peer holds the device key and that the relay did not
        // swap either ephemeral key or downgrade the version.
        let expectedMAC = RelayHandshake.transcriptMAC(
            e2eKey: e2eKey,
            deviceID: deviceID,
            clientSalt: clientSalt,
            hostSalt: hostSalt,
            clientEphemeralPublicKey: clientEphemeral.publicKey,
            hostEphemeralPublicKey: hostEphemeral
        )
        guard RelayHandshake.constantTimeEqual(hostMAC, expectedMAC) else {
            task.cancel(with: .policyViolation, reason: nil)
            throw URLError(.secureConnectionFailed)
        }
        let sharedSecret = try RelayHandshake.sharedSecret(
            privateKey: clientEphemeral.privateKey,
            peerPublicKey: hostEphemeral
        )
        let crypto = try RelayCryptoSession(
            e2eKey: e2eKey,
            sharedSecret: sharedSecret,
            clientSalt: clientSalt,
            hostSalt: hostSalt,
            isHost: false
        )
        return EstablishedConnection(socket: task, crypto: crypto)
    }

    /// Keepalive interval. Cloudflare answers WS pings at the edge, so a pong
    /// proves the Controller→relay path (the goal here: catching this
    /// device's own dead network path), keeps NAT/carrier mappings warm, and
    /// costs the Durable Object nothing.
    private static let keepalivePingInterval: TimeInterval = 15
    /// Hard silence deadline: with pings every 15s, a live transport is never
    /// quiet this long. `sendPing`'s callback alone is not enough — on a
    /// black-holed connection the ping buffers locally and the callback may
    /// simply never fire.
    private static let keepaliveSilenceLimit: TimeInterval = 40

    private func startPingLoop(socket task: URLSessionWebSocketTask, generation: UInt64) {
        pingTask?.cancel()
        pingTask = Task { [weak self] in
            while !Task.isCancelled {
                try? await Task.sleep(
                    nanoseconds: UInt64(Self.keepalivePingInterval * 1_000_000_000)
                )
                guard let self, !Task.isCancelled else { return }
                guard await self.keepaliveTick(socket: task, generation: generation) else {
                    return
                }
            }
        }
    }

    /// One keepalive beat: enforce the silence deadline, then ping. Returns
    /// false once this socket is no longer the current connection.
    private func keepaliveTick(
        socket task: URLSessionWebSocketTask,
        generation: UInt64
    ) -> Bool {
        guard generation == connectionGeneration, task === socket else { return false }
        if Date().timeIntervalSince(lastIncomingAt) > Self.keepaliveSilenceLimit {
            teardown(error: URLError(.networkConnectionLost))
            return false
        }
        task.sendPing { [weak self] error in
            Task { [weak self] in
                if let error {
                    await self?.teardownIfCurrent(task, generation: generation, error: error)
                } else {
                    await self?.notePong(socket: task, generation: generation)
                }
            }
        }
        return true
    }

    private func notePong(socket task: URLSessionWebSocketTask, generation: UInt64) {
        guard generation == connectionGeneration, task === socket else { return }
        lastIncomingAt = Date()
    }

    private func startReceiveLoop(socket task: URLSessionWebSocketTask, generation: UInt64) {
        receiveTask = Task { [weak self] in
            while !Task.isCancelled {
                do {
                    let message = try await task.receive()
                    guard case .data(let data) = message else { continue }
                    await self?.handleIncoming(data, socket: task, generation: generation)
                } catch {
                    await self?.teardownIfCurrent(task, generation: generation, error: error)
                    return
                }
            }
        }
    }

    private func teardownIfCurrent(
        _ task: URLSessionWebSocketTask,
        generation: UInt64,
        error: Error
    ) {
        guard generation == connectionGeneration, task === socket else { return }
        teardown(error: error)
    }

    private func handleIncoming(
        _ data: Data,
        socket task: URLSessionWebSocketTask,
        generation: UInt64
    ) {
        guard generation == connectionGeneration, task === socket else { return }
        lastIncomingAt = Date()
        guard var crypto else { return }
        guard let plaintext = try? crypto.open(data) else {
            // AEAD/replay failure is terminal — never skip a frame.
            teardown(error: URLError(.secureConnectionFailed))
            return
        }
        self.crypto = crypto
        // Responses always carry id+status; push frames always carry
        // stream+offset — the failed first decode falls through cleanly.
        if let response = try? JSONDecoder().decode(RelayTunnelResponse.self, from: plaintext) {
            settle(id: response.id, with: .success(response))
            return
        }
        if let push = try? JSONDecoder().decode(RelayStreamPush.self, from: plaintext) {
            pushContinuation?.yield(push)
        }
    }

    private static func receiveData(
        from task: URLSessionWebSocketTask,
        timeout: TimeInterval
    ) async throws -> Data {
        try await withThrowingTaskGroup(of: Data.self) { group in
            group.addTask {
                while true {
                    if case .data(let data) = try await task.receive() { return data }
                }
            }
            group.addTask {
                try await Task.sleep(nanoseconds: UInt64(timeout * 1_000_000_000))
                throw URLError(.timedOut)
            }
            guard let first = try await group.next() else { throw URLError(.timedOut) }
            group.cancelAll()
            return first
        }
    }

    private static func timeoutNanoseconds(_ timeout: TimeInterval) -> UInt64 {
        let seconds = max(1, timeout)
        let maximumSeconds = Double(UInt64.max) / 1_000_000_000
        return UInt64(min(seconds, maximumSeconds) * 1_000_000_000)
    }

    /// Fail CLOSED: a salt from a degraded RNG could, combined with a
    /// relay-replayed peer salt, weaken key derivation — so a failed draw
    /// aborts the handshake rather than proceeding with zero/partial bytes.
    private static func randomBytes(_ count: Int) throws -> Data {
        var bytes = [UInt8](repeating: 0, count: count)
        guard SecRandomCopyBytes(kSecRandomDefault, count, &bytes) == errSecSuccess else {
            throw URLError(.secureConnectionFailed)
        }
        return Data(bytes)
    }
}
