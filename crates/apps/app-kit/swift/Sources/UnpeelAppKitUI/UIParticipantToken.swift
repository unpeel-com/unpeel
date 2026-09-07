import CryptoKit
import Foundation

public enum UIParticipantToken {
    public static let version: UInt32 = 1
    public static let prefix = "upui1"
}

/// Stable signed claims shared with the Rust App endpoint.
public struct UIParticipantTokenClaims: Codable, Equatable, Sendable {
    public let version: UInt32
    public let appSessionID: String
    public let participant: UIParticipant
    public let clientID: String
    public let rendererID: String
    public let viewID: String
    public let issuedAtUnixMs: UInt64
    public let expiresAtUnixMs: UInt64
    public let tokenID: String

    public init(
        appSessionID: String,
        participant: UIParticipant,
        clientID: String,
        rendererID: String,
        viewID: String,
        issuedAtUnixMs: UInt64,
        expiresAtUnixMs: UInt64,
        tokenID: String
    ) {
        version = UIParticipantToken.version
        self.appSessionID = appSessionID
        self.participant = participant
        self.clientID = clientID
        self.rendererID = rendererID
        self.viewID = viewID
        self.issuedAtUnixMs = issuedAtUnixMs
        self.expiresAtUnixMs = expiresAtUnixMs
        self.tokenID = tokenID
    }

    enum CodingKeys: String, CodingKey {
        case version
        case appSessionID = "appSessionId"
        case participant
        case clientID = "clientId"
        case rendererID = "rendererId"
        case viewID = "viewId"
        case issuedAtUnixMs
        case expiresAtUnixMs
        case tokenID = "tokenId"
    }
}

public enum UIParticipantTokenError: Error, Equatable, LocalizedError {
    case signingKeyTooShort
    case invalidLifetime
    case invalidClaims
    case encodingFailed

    public var errorDescription: String? {
        switch self {
        case .signingKeyTooShort:
            "UI participant signing key must contain at least 32 bytes"
        case .invalidLifetime:
            "UI participant token lifetime is invalid"
        case .invalidClaims:
            "UI participant token claims are invalid"
        case .encodingFailed:
            "UI participant token could not be encoded"
        }
    }
}

/// Native Host helper. The signing key is the Host-retained value corresponding
/// to the `UNPEEL_UI_TOKEN` injected into the App; only the derived route-bound
/// token enters `UIAttach`.
public struct UIParticipantTokenIssuer: Sendable {
    private let signingKey: Data
    public let appSessionID: String

    public init(signingKey: Data, appSessionID: String) throws {
        guard signingKey.count >= 32 else {
            throw UIParticipantTokenError.signingKeyTooShort
        }
        self.signingKey = signingKey
        self.appSessionID = appSessionID
    }

    public init(signingKey: String, appSessionID: String) throws {
        try self.init(signingKey: Data(signingKey.utf8), appSessionID: appSessionID)
    }

    public func issue(
        participant: UIParticipant,
        clientID: String,
        rendererID: String,
        viewID: String,
        tokenID: String,
        validFor: TimeInterval = 300,
        now: Date = Date()
    ) throws -> String {
        guard validFor > 0,
              validFor.isFinite,
              now.timeIntervalSince1970 >= 0
        else {
            throw UIParticipantTokenError.invalidLifetime
        }
        let issued = UInt64((now.timeIntervalSince1970 * 1_000).rounded(.down))
        let validity = UInt64((validFor * 1_000).rounded(.down))
        let (expires, overflow) = issued.addingReportingOverflow(validity)
        guard validity > 0, !overflow else {
            throw UIParticipantTokenError.invalidLifetime
        }
        return try sign(UIParticipantTokenClaims(
            appSessionID: appSessionID,
            participant: participant,
            clientID: clientID,
            rendererID: rendererID,
            viewID: viewID,
            issuedAtUnixMs: issued,
            expiresAtUnixMs: expires,
            tokenID: tokenID
        ))
    }

    public func sign(_ claims: UIParticipantTokenClaims) throws -> String {
        guard claims.appSessionID == appSessionID,
              claims.expiresAtUnixMs > claims.issuedAtUnixMs
        else {
            throw UIParticipantTokenError.invalidLifetime
        }
        if claims.participant.kind == .agent,
           claims.participant.sourceSessionID == nil
        {
            throw UIParticipantTokenError.invalidClaims
        }
        let encoder = JSONEncoder()
        encoder.outputFormatting = [.sortedKeys, .withoutEscapingSlashes]
        let payload: Data
        do {
            payload = try encoder.encode(claims)
        } catch {
            throw UIParticipantTokenError.encodingFailed
        }
        let encodedPayload = payload.base64URLEncodedString()
        let signingInput = "\(UIParticipantToken.prefix).\(encodedPayload)"
        let authentication = HMAC<SHA256>.authenticationCode(
            for: Data(signingInput.utf8),
            using: SymmetricKey(data: signingKey)
        )
        return "\(signingInput).\(Data(authentication).base64URLEncodedString())"
    }
}

private extension Data {
    func base64URLEncodedString() -> String {
        base64EncodedString()
            .replacingOccurrences(of: "+", with: "-")
            .replacingOccurrences(of: "/", with: "_")
            .replacingOccurrences(of: "=", with: "")
    }
}
