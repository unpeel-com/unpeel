//
//  NativeRelayBridge.swift
//  UnpeelNative
//
//  Blocking C-callback adapter from Rust's transport-neutral HostConnection
//  to the canonical shared Swift RemoteRelayConnection. NativeRemoteBackend
//  invokes Rust away from the main actor, so waiting here never blocks UI.
//

import CUnpeelNativeBridge
import Dispatch
import Foundation
import UnpeelShared

final class NativeRelayBridgeContext: @unchecked Sendable {
    let connection: RemoteRelayConnection

    init(credentials: RelayCredentials, deviceID: String) {
        connection = RemoteRelayConnection(
            credentials: credentials,
            deviceID: deviceID
        )
    }

    func closeBlocking() {
        let completion = DispatchSemaphore(value: 0)
        let connection = connection
        Task.detached(priority: .utility) {
            await connection.close()
            completion.signal()
        }
        _ = completion.wait(timeout: .now() + .seconds(5))
    }
}

private final class RelayCallbackResultBox: @unchecked Sendable {
    private let lock = NSLock()
    private var value: Result<RemoteRelayTransportResponse, Error>?

    func store(_ result: Result<RemoteRelayTransportResponse, Error>) {
        lock.lock()
        value = result
        lock.unlock()
    }

    func take() -> Result<RemoteRelayTransportResponse, Error>? {
        lock.lock()
        defer { lock.unlock() }
        defer { value = nil }
        return value
    }
}

private func relayReturnBytes(
    _ data: Data,
    outPointer: UnsafeMutablePointer<UnsafeMutablePointer<UInt8>?>?,
    outLength: UnsafeMutablePointer<Int>?
) {
    guard let outPointer, let outLength else { return }
    outPointer.pointee = nil
    outLength.pointee = 0
    guard !data.isEmpty else { return }
    let pointer = UnsafeMutablePointer<UInt8>.allocate(capacity: data.count)
    data.copyBytes(to: pointer, count: data.count)
    outPointer.pointee = pointer
    outLength.pointee = data.count
}

private func relayReturnError(
    _ code: Int32,
    message: String,
    outPointer: UnsafeMutablePointer<UnsafeMutablePointer<UInt8>?>?,
    outLength: UnsafeMutablePointer<Int>?
) -> Int32 {
    relayReturnBytes(
        Data(message.utf8),
        outPointer: outPointer,
        outLength: outLength
    )
    return code
}

private func nativeRelayRequestCallback(
    _ opaqueContext: UnsafeMutableRawPointer?,
    _ requestPointer: UnsafePointer<UInt8>?,
    _ requestLength: Int,
    _ requiredGeneration: UInt64,
    _ timeoutMilliseconds: UInt64,
    _ outGeneration: UnsafeMutablePointer<UInt64>?,
    _ outPointer: UnsafeMutablePointer<UnsafeMutablePointer<UInt8>?>?,
    _ outLength: UnsafeMutablePointer<Int>?
) -> Int32 {
    guard let opaqueContext,
          let outGeneration,
          let outPointer,
          let outLength,
          requestLength == 0 || requestPointer != nil
    else {
        return Int32(UNPEEL_NATIVE_BRIDGE_RELAY_NOT_SENT)
    }
    outGeneration.pointee = 0
    outPointer.pointee = nil
    outLength.pointee = 0

    let requestData = requestLength == 0
        ? Data()
        : Data(bytes: requestPointer!, count: requestLength)
    let request: RelayTunnelRequest
    do {
        request = try JSONDecoder().decode(RelayTunnelRequest.self, from: requestData)
    } catch {
        return relayReturnError(
            Int32(UNPEEL_NATIVE_BRIDGE_RELAY_NOT_SENT),
            message: "Link request envelope was invalid.",
            outPointer: outPointer,
            outLength: outLength
        )
    }

    let context = Unmanaged<NativeRelayBridgeContext>
        .fromOpaque(opaqueContext)
        .takeUnretainedValue()
    let resultBox = RelayCallbackResultBox()
    let completion = DispatchSemaphore(value: 0)
    let connection = context.connection
    let timeout = max(1, TimeInterval(timeoutMilliseconds) / 1_000)
    let expected = requiredGeneration == 0 ? nil : requiredGeneration
    Task.detached(priority: .userInitiated) {
        do {
            let response = try await connection.perform(
                request: request,
                requiredConnectionGeneration: expected,
                timeout: timeout
            )
            resultBox.store(.success(response))
        } catch {
            resultBox.store(.failure(error))
        }
        completion.signal()
    }

    let dispatchWaitMilliseconds = NativeRelayBridge.callbackWaitMilliseconds(
        requestTimeoutMilliseconds: timeoutMilliseconds,
        mayEstablishConnection: requiredGeneration == 0
    )
    guard completion.wait(
        timeout: .now() + .milliseconds(dispatchWaitMilliseconds)
    ) == .success,
    let result = resultBox.take()
    else {
        return relayReturnError(
            Int32(UNPEEL_NATIVE_BRIDGE_RELAY_TIMED_OUT_OUTCOME_UNKNOWN),
            message: "Link request timed out.",
            outPointer: outPointer,
            outLength: outLength
        )
    }

    switch result {
    case let .success(transport):
        let encoded: Data
        do {
            encoded = try JSONEncoder().encode(transport.response)
        } catch {
            return relayReturnError(
                Int32(UNPEEL_NATIVE_BRIDGE_RELAY_OUTCOME_UNKNOWN),
                message: "Link returned an invalid response.",
                outPointer: outPointer,
                outLength: outLength
            )
        }
        outGeneration.pointee = transport.connectionGeneration
        relayReturnBytes(encoded, outPointer: outPointer, outLength: outLength)
        return Int32(UNPEEL_NATIVE_BRIDGE_RELAY_OK)
    case let .failure(error as RemoteRelayConnectionError):
        switch error {
        case .generationChanged:
            return relayReturnError(
                Int32(UNPEEL_NATIVE_BRIDGE_RELAY_GENERATION_CHANGED),
                message: error.localizedDescription,
                outPointer: outPointer,
                outLength: outLength
            )
        case let .transport(delivery, message):
            let code = delivery == .notSent
                ? UNPEEL_NATIVE_BRIDGE_RELAY_NOT_SENT
                : UNPEEL_NATIVE_BRIDGE_RELAY_OUTCOME_UNKNOWN
            return relayReturnError(
                Int32(code),
                message: message,
                outPointer: outPointer,
                outLength: outLength
            )
        case let .timedOut(delivery):
            let code = delivery == .notSent
                ? UNPEEL_NATIVE_BRIDGE_RELAY_TIMED_OUT_NOT_SENT
                : UNPEEL_NATIVE_BRIDGE_RELAY_TIMED_OUT_OUTCOME_UNKNOWN
            return relayReturnError(
                Int32(code),
                message: error.localizedDescription,
                outPointer: outPointer,
                outLength: outLength
            )
        }
    case let .failure(error):
        return relayReturnError(
            Int32(UNPEEL_NATIVE_BRIDGE_RELAY_OUTCOME_UNKNOWN),
            message: error.localizedDescription,
            outPointer: outPointer,
            outLength: outLength
        )
    }
}

private func nativeRelayBytesReleaseCallback(
    _: UnsafeMutableRawPointer?,
    _ pointer: UnsafeMutablePointer<UInt8>?,
    _: Int
) {
    pointer?.deallocate()
}

private func nativeRelayDisconnectCallback(_ opaqueContext: UnsafeMutableRawPointer?) {
    guard let opaqueContext else { return }
    Unmanaged<NativeRelayBridgeContext>
        .fromOpaque(opaqueContext)
        .takeUnretainedValue()
        .closeBlocking()
}

private func nativeRelayContextReleaseCallback(_ opaqueContext: UnsafeMutableRawPointer?) {
    guard let opaqueContext else { return }
    let context = Unmanaged<NativeRelayBridgeContext>
        .fromOpaque(opaqueContext)
        .takeRetainedValue()
    context.closeBlocking()
}

enum NativeRelayBridge {
    /// URLSession's WebSocket connect and the authenticated E2E host-hello
    /// receive can each consume ten seconds in the shared connection. Only an
    /// unconstrained bootstrap can pay that worst-case establishment cost; effects are
    /// generation-bound and therefore get scheduling headroom only. This
    /// watchdog must sit outside the actor's own delivery timeout, otherwise
    /// a slow but healthy first Link request is misclassified as ambiguous.
    static let connectionEstablishmentHeadroomMilliseconds: UInt64 = 20_000
    static let callbackSchedulingHeadroomMilliseconds: UInt64 = 5_000

    static func callbackWaitMilliseconds(
        requestTimeoutMilliseconds: UInt64,
        mayEstablishConnection: Bool
    ) -> Int {
        let establishment = mayEstablishConnection
            ? connectionEstablishmentHeadroomMilliseconds
            : 0
        let (headroom, headroomOverflow) = establishment.addingReportingOverflow(
            callbackSchedulingHeadroomMilliseconds
        )
        let (total, totalOverflow) = requestTimeoutMilliseconds.addingReportingOverflow(headroom)
        let milliseconds = headroomOverflow || totalOverflow
            ? UInt64.max
            : max(1_000, total)
        return Int(min(milliseconds, UInt64(Int.max)))
    }

    static func open(
        credentials: RelayCredentials,
        deviceID: String,
        authToken: String
    ) throws -> unpeel_native_bridge_remote_handle_t {
        let context = NativeRelayBridgeContext(
            credentials: credentials,
            deviceID: deviceID
        )
        let opaqueContext = Unmanaged.passRetained(context).toOpaque()
        let bearer = Data(authToken.utf8)
        var openedHandle: unpeel_native_bridge_remote_handle_t = 0
        var outputPointer: UnsafeMutablePointer<UInt8>?
        var outputLength = 0
        let result = bearer.withUnsafeBytes { bearerBytes in
            unpeel_native_bridge_remote_relay_open(
                bearerBytes.bindMemory(to: UInt8.self).baseAddress,
                bearerBytes.count,
                opaqueContext,
                nativeRelayRequestCallback,
                nativeRelayBytesReleaseCallback,
                nativeRelayDisconnectCallback,
                nativeRelayContextReleaseCallback,
                &openedHandle,
                &outputPointer,
                &outputLength
            )
        }
        let output = NativeRemoteBackend.takeBridgeOutput(
            outputPointer,
            length: outputLength
        )
        guard result == UNPEEL_NATIVE_BRIDGE_OK, openedHandle != 0 else {
            throw NativeRemoteBackend.bridgeFailure(
                result: result,
                output: output,
                fallbackCode: "remote_link_open_failed",
                fallbackMessage: "Could not open Unpeel Link."
            )
        }
        return openedHandle
    }
}
