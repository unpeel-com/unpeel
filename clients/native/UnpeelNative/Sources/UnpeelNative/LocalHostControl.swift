import CUnpeelNativeBridge
import Foundation
import UnpeelShared

/// Same-user administrative client for the canonical workspace worker. These
/// verbs are intentionally local-only and travel over the mode-0600
/// `host.sock`; the native app never edits Host authorization files itself in
/// client-only mode.
enum LocalHostControl {
    struct Snapshot: Sendable {
        let devices: [RemotePairedDeviceSummary]
        let directEndpoint: URL?
    }

    enum PairingStatus: String, Decodable, Sendable {
        case active
        case completed
        case closed
    }

    struct Error: LocalizedError, Sendable {
        let message: String
        var errorDescription: String? { message }
    }

    private struct Request: Encodable, Sendable {
        let action: String
        var advertisedHost: String? = nil
        var advertisedPort: UInt16? = nil
        var deviceID: String? = nil
        var allowed: Bool? = nil
    }

    private struct Config: Encodable, Sendable {
        let unpeelHome: String
        let request: Request
    }

    private struct PairingResponse: Decodable {
        let code: String
    }

    private struct StatusResponse: Decodable {
        let status: PairingStatus
    }

    private struct DevicesResponse: Decodable {
        let devices: [RemotePairedDeviceSummary]
        let directEndpoint: URL?
    }

    private struct BridgeError: Decodable {
        let message: String?
    }

    static func beginPairing(home: String) async throws -> RemotePairingPayload {
        let data = try await call(
            home: home,
            request: Request(action: "begin")
        )
        let response = try decode(PairingResponse.self, from: data)
        guard let payload = RemotePairingCode.decode(response.code) else {
            throw Error(message: "The workspace Host returned an invalid pairing code.")
        }
        return payload
    }

    static func pairingStatus(home: String) async throws -> PairingStatus {
        let data = try await call(
            home: home,
            request: Request(action: "status")
        )
        return try decode(StatusResponse.self, from: data).status
    }

    static func cancelPairing(home: String) async throws {
        _ = try await call(
            home: home,
            request: Request(action: "cancel")
        )
    }

    static func snapshot(home: String) async throws -> Snapshot {
        let data = try await call(
            home: home,
            request: Request(action: "devices")
        )
        let response = try decode(DevicesResponse.self, from: data)
        return Snapshot(devices: response.devices, directEndpoint: response.directEndpoint)
    }

    static func revokeDevice(home: String, id: String) async throws {
        _ = try await call(
            home: home,
            request: Request(action: "revoke-device", deviceID: id)
        )
    }

    static func setRelayAllowed(home: String, id: String, allowed: Bool) async throws {
        _ = try await call(
            home: home,
            request: Request(
                action: "set-relay-allowed",
                deviceID: id,
                allowed: allowed
            )
        )
    }

    /// Launch-time liveness probe: one `devices` round trip over host.sock.
    /// True only when a worker for `home` accepted and answered the request.
    nonisolated static func probeBlocking(home: String) -> Bool {
        guard let config = try? JSONEncoder().encode(
            Config(unpeelHome: home, request: Request(action: "devices"))
        ) else { return false }
        return (try? callBlocking(config)) != nil
    }

    private static func call(home: String, request: Request) async throws -> Data {
        let preparation = await MainActor.run { HostServiceManager.shared.launchPreparation }
        await preparation?.value
        try Task.checkCancellation()
        let config: Data
        do {
            config = try JSONEncoder().encode(Config(unpeelHome: home, request: request))
        } catch {
            throw Error(message: "Could not encode the workspace Host request.")
        }
        return try await Task.detached(priority: .userInitiated) {
            try callBlocking(config)
        }.value
    }

    private nonisolated static func callBlocking(_ config: Data) throws -> Data {
        var outputPointer: UnsafeMutablePointer<UInt8>?
        var outputLength = 0
        let result = config.withUnsafeBytes { bytes in
            unpeel_native_bridge_local_host_control(
                bytes.bindMemory(to: UInt8.self).baseAddress,
                bytes.count,
                &outputPointer,
                &outputLength
            )
        }
        let output: Data
        if let outputPointer, outputLength > 0 {
            output = Data(bytes: outputPointer, count: outputLength)
            unpeel_native_bridge_free(outputPointer, outputLength)
        } else {
            output = Data()
        }
        guard result == UNPEEL_NATIVE_BRIDGE_OK else {
            let message = (try? JSONDecoder().decode(BridgeError.self, from: output).message)
                ?? "The workspace Host rejected the request."
            throw Error(message: message)
        }
        return output
    }

    private static func decode<Value: Decodable>(
        _ type: Value.Type,
        from data: Data
    ) throws -> Value {
        do {
            return try JSONDecoder().decode(type, from: data)
        } catch {
            throw Error(message: "The workspace Host returned an invalid response.")
        }
    }
}
