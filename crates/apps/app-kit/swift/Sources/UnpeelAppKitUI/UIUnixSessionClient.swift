import Foundation
import Network

/// The wire representation used for the most recent accepted projection.
///
/// `UIUnixSessionClient` always exposes a complete `UISnapshot` to renderers,
/// including after it applies a delta. This separate signal lets Host test
/// rigs verify whether the App actually sent a snapshot or a compact delta.
public enum UIProjectionDelivery: Equatable, Sendable {
    case snapshot(revision: Int)
    case delta(baseRevision: Int, revision: Int, operationCount: Int)
}

private extension Optional where Wrapped == UIAction {
    func isSomeCoalescingPeer(of other: UIAction) -> Bool {
        guard let action = self else { return false }
        return action.nodeID == other.nodeID
            && action.action == other.action
            && action.kind == other.kind
    }
}

/// Reconnecting native client for an App-owned `unpeel.ui/1` Unix socket.
///
/// This class belongs in the trusted native Host. Never instantiate it in web
/// content or disclose a scoped participant token to a WebView.
public final class UIUnixSessionClient: @unchecked Sendable {
    public struct Configuration: Sendable {
        public let socketPath: String
        public let participantTokenProvider: @Sendable () throws -> String
        public let clientID: String
        public let renderer: UIRendererMetadata
        public let viewID: String
        public let supportedComponentCapabilities: [String]

        public init(
            socketPath: String,
            participantToken: String,
            clientID: String,
            renderer: UIRendererMetadata,
            viewID: String,
            supportedComponentCapabilities: [String] = UnpeelUIProtocol.supportedComponentCapabilities
        ) {
            self.socketPath = socketPath
            participantTokenProvider = { participantToken }
            self.clientID = clientID
            self.renderer = renderer
            self.viewID = viewID
            self.supportedComponentCapabilities = supportedComponentCapabilities
        }

        /// A provider lets the Host mint a fresh short-lived token on reconnect.
        public init(
            socketPath: String,
            participantTokenProvider: @escaping @Sendable () throws -> String,
            clientID: String,
            renderer: UIRendererMetadata,
            viewID: String,
            supportedComponentCapabilities: [String] = UnpeelUIProtocol.supportedComponentCapabilities
        ) {
            self.socketPath = socketPath
            self.participantTokenProvider = participantTokenProvider
            self.clientID = clientID
            self.renderer = renderer
            self.viewID = viewID
            self.supportedComponentCapabilities = supportedComponentCapabilities
        }
    }

    public enum ConnectionState: Equatable, Sendable {
        case stopped
        case connecting
        case attached(appInstanceID: String, resumed: Bool)
        case waitingToReconnect
    }

    private static let maximumFrameBytes = 16 * 1_024 * 1_024

    private let configuration: Configuration
    private let queue: DispatchQueue
    private let onMessage: @Sendable (UIMessage) -> Void
    private let onProjectionDelivery: @Sendable (UIProjectionDelivery) -> Void
    private let onState: @Sendable (ConnectionState) -> Void
    private let onTerminalFallback: @Sendable (String) -> Void
    private var connection: NWConnection?
    private var receiveBuffer = Data()
    private var running = false
    private var ready = false
    private var reconnectAttempt = 0
    private var rendererState = UIRendererState.terminal
    private var negotiatedProtocolVersion: Int?
    private var appInstanceID: String?
    private var participantID: String?
    private var latestSnapshot: UISnapshot?
    private var semanticProjectionAvailable = false
    private var usingTerminalFallback = false
    private var rendererStateBeforeFallback: UIRendererState?
    private var pendingActions: [String: UIAction] = [:]
    private var pendingEvents: [String: UIEvent] = [:]
    private var pendingEventOrder: [String] = []
    private var inFlightEventID: String?

    public init(
        configuration: Configuration,
        onMessage: @escaping @Sendable (UIMessage) -> Void,
        onProjectionDelivery: @escaping @Sendable (UIProjectionDelivery) -> Void = { _ in },
        onState: @escaping @Sendable (ConnectionState) -> Void = { _ in },
        onTerminalFallback: @escaping @Sendable (String) -> Void = { _ in }
    ) {
        self.configuration = configuration
        self.onMessage = onMessage
        self.onProjectionDelivery = onProjectionDelivery
        self.onState = onState
        self.onTerminalFallback = onTerminalFallback
        queue = DispatchQueue(label: "com.unpeel.app-kit.ui.\(configuration.clientID)")
    }

    public func start(rendererState: UIRendererState = .terminal) {
        queue.async { [weak self] in
            guard let self, !running else { return }
            running = true
            self.rendererState = rendererState
            connect()
        }
    }

    public func stop() {
        queue.async { [weak self] in
            guard let self else { return }
            running = false
            ready = false
            semanticProjectionAvailable = false
            negotiatedProtocolVersion = nil
            connection?.stateUpdateHandler = nil
            connection?.cancel()
            connection = nil
            onState(.stopped)
        }
    }

    /// Wraps a renderer-local action in authenticated session identity.
    public func send(_ action: UIAction, eventID: String = UUID().uuidString.lowercased()) {
        queue.async { [weak self] in
            guard let self, semanticProjectionAvailable else { return }
            if let existing = pendingEvents[eventID] {
                if ready, inFlightEventID == eventID {
                    sendMessage(.event(existing))
                }
                return
            }
            if pendingActions[eventID] != nil { return }
            if Self.canCoalesce(action),
               let queuedID = pendingEventOrder.reversed().first(where: { candidate in
                   pendingEvents[candidate] == nil
                       && pendingActions[candidate].isSomeCoalescingPeer(of: action)
               })
            {
                pendingActions[queuedID] = action
                return
            }
            pendingActions[eventID] = action
            pendingEventOrder.append(eventID)
            dispatchNextAction()
        }
    }

    public func setRendererState(_ state: UIRendererState) {
        queue.async { [weak self] in
            guard let self else { return }
            rendererState = state
            guard ready, let snapshot = latestSnapshot else { return }
            sendMessage(.lifecycle(UILifecycle(
                snapshot: snapshot,
                rendererID: configuration.renderer.id,
                state: state
            )))
        }
    }

    public func requestSnapshot() {
        queue.async { [weak self] in
            guard let self, ready,
                  let appInstanceID
            else { return }
            sendMessage(.requestSnapshot(UIRequestSnapshot(
                protocolName: UnpeelUIProtocol.name,
                protocolVersion: negotiatedProtocolVersion ?? UnpeelUIProtocol.version,
                appInstanceID: appInstanceID,
                clientID: configuration.clientID,
                rendererID: configuration.renderer.id,
                viewID: configuration.viewID
            )))
        }
    }

    private func connect() {
        guard running else { return }
        onState(.connecting)
        ready = false
        negotiatedProtocolVersion = nil
        receiveBuffer.removeAll(keepingCapacity: true)

        let connection = NWConnection(
            to: .unix(path: configuration.socketPath),
            using: .tcp
        )
        self.connection = connection
        connection.stateUpdateHandler = { [weak self, weak connection] state in
            guard let self, let connection else { return }
            handle(state, connection: connection)
        }
        connection.start(queue: queue)
    }

    private func handle(_ state: NWConnection.State, connection: NWConnection) {
        guard self.connection === connection else { return }
        switch state {
        case .ready:
            reconnectAttempt = 0
            sendAttach()
            receiveNext()
        case .failed, .cancelled:
            ready = false
            self.connection = nil
            scheduleReconnect()
        case .setup, .preparing, .waiting:
            break
        @unknown default:
            break
        }
    }

    private func sendAttach() {
        do {
            let capabilities = Array(Set(
                configuration.renderer.capabilities
                    + configuration.supportedComponentCapabilities
                    + [UnpeelUIProtocol.deltaCapability]
            )).sorted()
            let renderer = UIRendererMetadata(
                id: configuration.renderer.id,
                kind: configuration.renderer.kind,
                capabilities: capabilities
            )
            let attach = UIAttach(
                participantToken: try configuration.participantTokenProvider(),
                clientID: configuration.clientID,
                renderer: renderer,
                viewID: configuration.viewID,
                expectedAppInstanceID: appInstanceID,
                lastSeenRevision: latestSnapshot?.revision,
                state: rendererState
            )
            sendMessage(.attach(attach))
        } catch {
            connection?.cancel()
        }
    }

    private func receiveNext() {
        guard let connection else { return }
        connection.receive(
            minimumIncompleteLength: 1,
            maximumLength: 64 * 1_024
        ) { [weak self, weak connection] data, _, isComplete, error in
            guard let self, let connection, self.connection === connection else { return }
            if let data, !data.isEmpty {
                receiveBuffer.append(data)
                decodeAvailableFrames()
            }
            if isComplete || error != nil {
                ready = false
                connection.cancel()
                return
            }
            receiveNext()
        }
    }

    private func decodeAvailableFrames() {
        while let newline = receiveBuffer.firstIndex(of: 0x0A) {
            var frame = receiveBuffer[..<newline]
            receiveBuffer.removeSubrange(...newline)
            if frame.last == 0x0D {
                frame = frame.dropLast()
            }
            guard !frame.isEmpty, frame.count <= Self.maximumFrameBytes else {
                connection?.cancel()
                return
            }
            do {
                let message = try JSONDecoder().decode(UIMessage.self, from: Data(frame))
                handle(message)
            } catch {
                connection?.cancel()
                return
            }
        }
        if receiveBuffer.count > Self.maximumFrameBytes {
            connection?.cancel()
        }
    }

    private func handle(_ message: UIMessage) {
        if let negotiatedProtocolVersion,
           let messageVersion = message.protocolVersion,
           messageVersion != negotiatedProtocolVersion
        {
            connection?.cancel()
            return
        }
        switch message {
        case let .attached(attached):
            guard UnpeelUIProtocol.negotiate(
                    minimum: attached.minProtocolVersion,
                    maximum: attached.maxProtocolVersion
                  ) == attached.protocolVersion,
                  attached.clientID == configuration.clientID,
                  attached.rendererID == configuration.renderer.id,
                  attached.viewID == configuration.viewID
            else {
                connection?.cancel()
                return
            }
            let sameInstance = appInstanceID == nil || appInstanceID == attached.appInstanceID
            let sameParticipant = participantID == nil || participantID == attached.participantID
            if !sameInstance || !sameParticipant {
                pendingActions.removeAll()
                pendingEvents.removeAll()
                pendingEventOrder.removeAll()
                inFlightEventID = nil
                latestSnapshot = nil
                semanticProjectionAvailable = false
            }
            appInstanceID = attached.appInstanceID
            participantID = attached.participantID
            negotiatedProtocolVersion = attached.protocolVersion
            ready = true
            onState(.attached(
                appInstanceID: attached.appInstanceID,
                resumed: attached.resumed
            ))
            if let eventID = inFlightEventID,
               let event = pendingEvents[eventID]
            {
                // Replay exactly the already-sent envelope. The App's event-id
                // cache makes this safe whether the previous ack or action was
                // the frame lost during reconnect.
                sendMessage(.event(event))
            }
        case let .snapshot(snapshot):
            guard snapshot.protocolVersion == negotiatedProtocolVersion,
                  snapshot.clientID == configuration.clientID,
                  snapshot.viewID == configuration.viewID,
                  snapshot.appInstanceID == appInstanceID
            else { return }
            guard accept(snapshot) else { return }
            onProjectionDelivery(.snapshot(revision: snapshot.revision))
        case let .delta(delta):
            guard delta.protocolVersion == negotiatedProtocolVersion else { return }
            guard let snapshot = latestSnapshot else {
                requestSnapshot()
                return
            }
            do {
                let next = try snapshot.applying(delta)
                if accept(next) {
                    onProjectionDelivery(.delta(
                        baseRevision: delta.baseRevision,
                        revision: delta.revision,
                        operationCount: delta.operations.count
                    ))
                    onMessage(.snapshot(next))
                }
            } catch {
                requestSnapshot()
            }
            return
        case let .ack(ack):
            guard ack.protocolVersion == negotiatedProtocolVersion,
                  ack.clientID == configuration.clientID,
                  ack.rendererID == configuration.renderer.id
            else { return }
            if ack.status != .pending {
                pendingActions.removeValue(forKey: ack.eventID)
                pendingEvents.removeValue(forKey: ack.eventID)
                pendingEventOrder.removeAll { $0 == ack.eventID }
                if inFlightEventID == ack.eventID {
                    inFlightEventID = nil
                }
                if ack.status == .stale {
                    requestSnapshot()
                } else {
                    dispatchNextAction()
                }
            }
        case .error:
            break
        case .attach, .event, .lifecycle, .requestSnapshot:
            connection?.cancel()
            return
        case let .presence(presence):
            guard presence.protocolVersion == negotiatedProtocolVersion,
                  presence.appInstanceID == appInstanceID,
                  presence.viewID == configuration.viewID
            else { return }
        }
        onMessage(message)
    }

    /// Keeps the attachment alive but asks the Host to expose the complete PTY
    /// when this renderer cannot safely map a component kind.
    private func accept(_ snapshot: UISnapshot) -> Bool {
        latestSnapshot = snapshot
        let component = snapshot.root.component
        if let capabilities = component.requiredCapabilities,
           capabilities.allSatisfy(configuration.supportedComponentCapabilities.contains)
        {
            semanticProjectionAvailable = true
            dispatchNextAction()
            if usingTerminalFallback {
                let restoredState = rendererStateBeforeFallback ?? .component
                usingTerminalFallback = false
                rendererStateBeforeFallback = nil
                if rendererState != restoredState {
                    rendererState = restoredState
                    if ready {
                        sendMessage(.lifecycle(UILifecycle(
                            snapshot: snapshot,
                            rendererID: configuration.renderer.id,
                            state: restoredState
                        )))
                    }
                }
            }
            return true
        }

        semanticProjectionAvailable = false
        if !usingTerminalFallback {
            rendererStateBeforeFallback = rendererState
            usingTerminalFallback = true
        }
        if rendererState != .terminal {
            rendererState = .terminal
            if ready {
                sendMessage(.lifecycle(UILifecycle(
                    snapshot: snapshot,
                    rendererID: configuration.renderer.id,
                    state: .terminal
                )))
            }
        }
        onTerminalFallback(component.kind)
        return false
    }

    private func dispatchNextAction() {
        guard ready, semanticProjectionAvailable, inFlightEventID == nil,
              let eventID = pendingEventOrder.first,
              let action = pendingActions[eventID],
              let snapshot = latestSnapshot,
              let participantID
        else { return }
        let event = UIEvent(
            snapshot: snapshot,
            participantID: participantID,
            rendererID: configuration.renderer.id,
            eventID: eventID,
            action: action
        )
        pendingEvents[eventID] = event
        inFlightEventID = eventID
        sendMessage(.event(event))
    }

    private static func canCoalesce(_ action: UIAction) -> Bool {
        action.kind == .change || action.kind == .select
    }

    private func sendMessage(_ message: UIMessage) {
        guard let connection else { return }
        do {
            var data = try JSONEncoder().encode(message)
            guard data.count <= Self.maximumFrameBytes else {
                connection.cancel()
                return
            }
            data.append(0x0A)
            connection.send(content: data, completion: .contentProcessed { [weak connection] error in
                if error != nil {
                    connection?.cancel()
                }
            })
        } catch {
            connection.cancel()
        }
    }

    private func scheduleReconnect() {
        guard running else {
            onState(.stopped)
            return
        }
        reconnectAttempt = min(reconnectAttempt + 1, 8)
        let delay = min(pow(2.0, Double(reconnectAttempt - 1)) * 0.1, 5.0)
        onState(.waitingToReconnect)
        queue.asyncAfter(deadline: .now() + delay) { [weak self] in
            guard let self, running, connection == nil else { return }
            connect()
        }
    }
}
