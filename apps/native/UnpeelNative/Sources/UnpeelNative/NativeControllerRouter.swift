//
//  NativeControllerRouter.swift
//  UnpeelNative
//
//  Thin JSON-only wrapper around the panic-contained Rust controller router.
//  The app uses it only for Controller-side reads (the artifact thumbnail
//  adapter's original-byte source); the principal is part of the envelope.
//

import CUnpeelNativeBridge
import Foundation

struct NativeControllerPrincipal: Equatable, Sendable {
    let deviceID: String
    let name: String
    let principalID: String?

    init(deviceID: String, name: String, principalID: String? = nil) {
        self.deviceID = deviceID
        self.name = name
        self.principalID = principalID
    }

    var jsonObject: [String: Any] {
        var object: [String: Any] = [
            "kind": "paired_device",
            "deviceId": deviceID,
            "name": name,
        ]
        if let principalID { object["principalId"] = principalID }
        return object
    }
}

struct NativeControllerResponse: Equatable, Sendable {
    let status: Int
    let body: String
}

enum NativeControllerRouteResult: Equatable, Sendable {
    case handled(NativeControllerResponse)
    case unhandled
    /// The Rust entry point was not called, so compatibility fallback cannot
    /// duplicate an effect (ABI skew or local request-encoding failure).
    case bridgeUnavailable(String)
    /// Rust was called and may have applied a mutation before failing. Callers
    /// must not replay a non-idempotent request through another adapter.
    case bridgeError(String)
}

protocol NativeControllerRouting: Sendable {
    func route(
        requestID: String?,
        method: String,
        path: String,
        query: [String: String],
        headers: [String: String],
        body: Data,
        principal: NativeControllerPrincipal,
        routeContext: Data?
    ) -> NativeControllerRouteResult
}

struct NativeControllerRouter: NativeControllerRouting {
    static let shared = NativeControllerRouter()
    static let supportedABIVersion: UInt32 = 1

    func route(
        requestID: String?,
        method: String,
        path: String,
        query: [String: String],
        headers: [String: String],
        body: Data,
        principal: NativeControllerPrincipal,
        routeContext: Data?
    ) -> NativeControllerRouteResult {
        guard unpeel_native_bridge_abi_version() == Self.supportedABIVersion else {
            return .bridgeUnavailable("unsupported native controller bridge ABI")
        }

        let semanticRequestID = requestID.flatMap { candidate in
            let bytes = candidate.utf8.count
            return bytes > 0 && bytes <= 128 ? candidate : nil
        } ?? UUID().uuidString.lowercased()
        var request: [String: Any] = [
            "id": semanticRequestID,
            "method": method,
            "path": path,
            "query": query,
            "body": NSNull(),
            "principal": principal.jsonObject,
        ]
        if let contentType = headers["content-type"], !contentType.isEmpty {
            request["contentType"] = contentType
        }
        if !body.isEmpty {
            if let json = try? JSONSerialization.jsonObject(with: body, options: [.fragmentsAllowed]) {
                request["body"] = json
            } else {
                request["bodyBase64"] = body.base64EncodedString()
            }
        }

        guard JSONSerialization.isValidJSONObject(request),
              let requestData = try? JSONSerialization.data(withJSONObject: request)
        else {
            return .bridgeUnavailable("could not encode controller request")
        }
        let contextData = routeContext ?? Data()

        var outputPointer: UnsafeMutablePointer<UInt8>?
        var outputLength = 0
        let result: Int32 = requestData.withUnsafeBytes { requestBytes in
            contextData.withUnsafeBytes { contextBytes in
                unpeel_native_bridge_route(
                    requestBytes.bindMemory(to: UInt8.self).baseAddress,
                    requestBytes.count,
                    contextBytes.bindMemory(to: UInt8.self).baseAddress,
                    contextBytes.count,
                    &outputPointer,
                    &outputLength
                )
            }
        }
        let output: Data
        if let outputPointer, outputLength > 0 {
            output = Data(bytes: outputPointer, count: outputLength)
            unpeel_native_bridge_free(outputPointer, outputLength)
        } else {
            output = Data()
        }

        if result == UNPEEL_NATIVE_BRIDGE_UNHANDLED {
            return .unhandled
        }
        guard result == UNPEEL_NATIVE_BRIDGE_HANDLED else {
            let message = Self.errorMessage(output)
                ?? "native controller bridge failed (\(result))"
            return .bridgeError(message)
        }
        guard let envelope = try? JSONSerialization.jsonObject(with: output) as? [String: Any],
              let status = (envelope["status"] as? NSNumber)?.intValue,
              let body = envelope["body"]
        else {
            return .bridgeError("invalid controller response")
        }
        guard let bodyData = try? JSONSerialization.data(
            withJSONObject: body,
            options: [.fragmentsAllowed]
        ), let bodyString = String(data: bodyData, encoding: .utf8) else {
            return .bridgeError("invalid controller response body")
        }
        return .handled(NativeControllerResponse(status: status, body: bodyString))
    }

    private static func errorMessage(_ data: Data) -> String? {
        guard let object = try? JSONSerialization.jsonObject(with: data) as? [String: Any]
        else { return nil }
        return object["error"] as? String
    }
}
