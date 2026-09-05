import Foundation

/// Failures produced by the sealed, one-time Controller-to-Host pairing
/// exchange. HTTP failures retain the Host's structured `{"error": ...}`
/// message so each frontend can present it using its own error type.
public enum RemotePairingClientError: Error, Equatable, Sendable {
    case expired
    case invalidHostIdentity
    case incompatibleProtocol
    case invalidHTTPResponse
    case httpStatus(statusCode: Int, serverMessage: String?)
    case responseHostIdentityMismatch
    case responseEndpointMismatch
    case responseDeviceIdentityMismatch
    case invalidCredentials
}

/// The transport-neutral pairing implementation shared by every Apple
/// Controller. Pairing itself is always a direct request to the endpoint in
/// the scanned code; confidentiality and Host identity come from the sealed
/// request/response exchange, not from trusting the HTTP bootstrap.
public struct RemotePairingClient: Sendable {
    typealias DataLoader = @Sendable (URLRequest) async throws -> (Data, URLResponse)

    private let dataLoader: DataLoader

    public init(session: URLSession = .shared) {
        dataLoader = { request in
            try await session.data(for: request)
        }
    }

    init(dataLoader: @escaping DataLoader) {
        self.dataLoader = dataLoader
    }

    public func pair(
        payload: RemotePairingPayload,
        device: RemoteDeviceIdentity,
        now: Date = Date()
    ) async throws -> RemotePairingResponse {
        let nowMs = Int64(now.timeIntervalSince1970 * 1_000)
        guard payload.expiresAtUnixMs > nowMs else {
            throw RemotePairingClientError.expired
        }
        guard !payload.macID.isEmpty else {
            throw RemotePairingClientError.invalidHostIdentity
        }

        let encoder = JSONEncoder()
        let pairRequest = try encoder.encode(
            RemotePairingRequest(token: payload.token, device: device)
        )
        let envelope = try RemotePairingCrypto.seal(
            pairRequest,
            token: payload.token,
            macID: payload.macID,
            endpoint: payload.endpoint,
            direction: .request
        )

        var request = URLRequest(url: payload.endpoint.appendingPathComponent("pair"))
        request.httpMethod = "POST"
        request.timeoutInterval = payload.endpoint.path.hasPrefix("/mobile/pairing-proxy/")
            ? 25 : 10
        request.setValue("application/json", forHTTPHeaderField: "Content-Type")
        request.httpBody = try encoder.encode(envelope)

        let (data, response) = try await dataLoader(request)
        try Task.checkCancellation()
        guard let http = response as? HTTPURLResponse else {
            throw RemotePairingClientError.invalidHTTPResponse
        }
        guard (200 ..< 300).contains(http.statusCode) else {
            throw RemotePairingClientError.httpStatus(
                statusCode: http.statusCode,
                serverMessage: Self.serverErrorMessage(from: data)
            )
        }

        let decoder = JSONDecoder()
        let responseEnvelope = try decoder.decode(RemotePairingEnvelope.self, from: data)
        let plaintext = try RemotePairingCrypto.open(
            responseEnvelope,
            token: payload.token,
            macID: payload.macID,
            endpoint: payload.endpoint,
            direction: .response
        )
        let paired = try decoder.decode(RemotePairingResponse.self, from: plaintext)
        guard paired.protocolVersion == RemoteControlProtocol.version else {
            throw RemotePairingClientError.incompatibleProtocol
        }
        guard paired.macID == payload.macID else {
            throw RemotePairingClientError.responseHostIdentityMismatch
        }
        guard paired.endpoint == payload.endpoint else {
            throw RemotePairingClientError.responseEndpointMismatch
        }
        if let directEndpoint = paired.directEndpoint {
            // A TLS-capable Host may advertise its Direct endpoint as https;
            // the Controller decides the scheme from the certificate pin, so
            // both spellings name the same `/mobile` port.
            guard let scheme = directEndpoint.scheme?.lowercased(),
                  scheme == "http" || scheme == "https",
                  directEndpoint.host != nil,
                  directEndpoint.port != nil,
                  directEndpoint.path == "/mobile",
                  directEndpoint.query == nil,
                  directEndpoint.fragment == nil
            else {
                throw RemotePairingClientError.invalidCredentials
            }
        }
        guard paired.deviceID == device.id else {
            throw RemotePairingClientError.responseDeviceIdentityMismatch
        }
        let relay = paired.relayCredentials
        guard !paired.authToken.isEmpty,
              relay.macID == payload.macID,
              !relay.relayToken.isEmpty,
              relay.relayURL.scheme?.lowercased() == "wss",
              relay.e2eKey?.count == 32
        else {
            throw RemotePairingClientError.invalidCredentials
        }
        return paired
    }

    private static func serverErrorMessage(from body: Data) -> String? {
        guard !body.isEmpty,
              let object = try? JSONSerialization.jsonObject(with: body) as? [String: Any],
              let message = object["error"] as? String,
              !message.isEmpty
        else { return nil }
        return message
    }
}
