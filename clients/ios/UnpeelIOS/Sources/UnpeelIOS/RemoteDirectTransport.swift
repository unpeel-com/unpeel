//
//  RemoteDirectTransport.swift
//  UnpeelIOS
//
//  Pure decisions behind the phone's Direct (LAN) `/mobile` transport and
//  the relay's request budget:
//
//  - which scheme a paired Host gets (pinned HTTPS vs legacy plaintext) and
//    how that decision is learned from bootstrap / pairing and persisted;
//  - the bootstrap deadline per transport (4 s health poll on the LAN, a
//    wider budget over the relay so one slow cellular round-trip cannot
//    feed the reconnect loop);
//  - the push-token registration route for each paired Mac.
//
//  Everything here is socket-free and unit-tested. The connection store
//  applies these decisions; RemoteMacClient executes them.
//

import Foundation
import UnpeelShared

/// Semantic server version (`"0.5.3"`, `"0.10.0-beta.2"`). Pre-release and
/// build suffixes are ignored: `0.5.3-beta.1` is 0.5.3 for gating purposes
/// because the feature ships with the release line, not the tag.
struct RemoteServerVersion: Equatable, Comparable, Sendable {
    let major: Int
    let minor: Int
    let patch: Int

    init(major: Int, minor: Int, patch: Int) {
        self.major = major
        self.minor = minor
        self.patch = patch
    }

    init?(_ raw: String?) {
        guard let raw else { return nil }
        var core = raw.trimmingCharacters(in: .whitespacesAndNewlines)
        if core.hasPrefix("v") || core.hasPrefix("V") { core.removeFirst() }
        if let cut = core.firstIndex(where: { $0 == "-" || $0 == "+" }) {
            core = String(core[..<cut])
        }
        let parts = core.split(separator: ".", omittingEmptySubsequences: false)
        guard (1...3).contains(parts.count) else { return nil }
        var numbers: [Int] = []
        for part in parts {
            guard let value = Int(part), value >= 0 else { return nil }
            numbers.append(value)
        }
        while numbers.count < 3 { numbers.append(0) }
        self.init(major: numbers[0], minor: numbers[1], patch: numbers[2])
    }

    static func < (lhs: RemoteServerVersion, rhs: RemoteServerVersion) -> Bool {
        (lhs.major, lhs.minor, lhs.patch) < (rhs.major, rhs.minor, rhs.patch)
    }
}

/// What a Host said about its Direct transport, extracted at the wire
/// boundary from a bootstrap snapshot or a sealed pairing response.
struct RemoteDirectTransportAdvertisement: Equatable, Sendable {
    /// Lowercase hex SHA-256 of the Host's self-signed TLS leaf — the same
    /// certificate on the `__remote__` WSS port and the `/mobile` port.
    let certificateFingerprint: String?
    let serverVersion: String?
    /// `hostProtocol.capabilities`; nil on a pre-ledger Host.
    let hostCapabilities: [String]?

    init(
        certificateFingerprint: String?,
        serverVersion: String?,
        hostCapabilities: [String]?
    ) {
        self.certificateFingerprint = certificateFingerprint
        self.serverVersion = serverVersion
        self.hostCapabilities = hostCapabilities
    }

    init(bootstrap snapshot: RemoteBootstrapSnapshot) {
        self.init(
            certificateFingerprint: snapshot.remoteServerCertificateFingerprint,
            serverVersion: snapshot.serverVersion,
            hostCapabilities: snapshot.hostProtocol?.capabilities
        )
    }

    init(pairing response: RemotePairingResponse) {
        self.init(
            certificateFingerprint: response.remoteServerCertificateFingerprint,
            serverVersion: response.serverVersion,
            hostCapabilities: nil
        )
    }
}

/// The scheme a paired Host's Direct `/mobile` requests use.
enum RemoteDirectTransportDecision: Equatable, Sendable {
    /// Pinned HTTPS. The bearer only ever rides this.
    case tls(fingerprint: String)
    /// The Host said it serves TLS but advertised no certificate to pin to.
    /// Stay on whatever the record already uses; never send the bearer to
    /// an unpinned TLS endpoint, and never assume plaintext is acceptable.
    case tlsUnpinnable
    /// The Host conclusively predates TLS on `/mobile` (a version below the
    /// minimum). Plaintext is the only transport it accepts.
    case plaintext
    /// The Host said nothing either way (pre-version, pre-ledger). Keep the
    /// record's current transport.
    case unknown
}

enum RemoteDirectTransportPolicy {
    static func normalizedFingerprint(_ raw: String?) -> String? {
        guard let raw else { return nil }
        let trimmed = raw.trimmingCharacters(in: .whitespacesAndNewlines).lowercased()
        return trimmed.isEmpty ? nil : trimmed
    }

    /// The capability flag wins outright; otherwise a reported server version
    /// at/after `mobileTLSMinimumServerVersion` means TLS and a lower one
    /// means plaintext. No signal keeps the current transport.
    static func decision(
        for advertisement: RemoteDirectTransportAdvertisement
    ) -> RemoteDirectTransportDecision {
        let fingerprint = normalizedFingerprint(advertisement.certificateFingerprint)
        if advertisement.hostCapabilities?.contains(RemoteControlProtocol.mobileTLSCapability) == true {
            return fingerprint.map { .tls(fingerprint: $0) } ?? .tlsUnpinnable
        }
        guard let version = RemoteServerVersion(advertisement.serverVersion),
              let minimum = RemoteServerVersion(RemoteControlProtocol.mobileTLSMinimumServerVersion)
        else { return .unknown }
        guard version >= minimum else { return .plaintext }
        return fingerprint.map { .tls(fingerprint: $0) } ?? .tlsUnpinnable
    }

    /// Fold a decision into a stored record. Returns nil when nothing changed.
    ///
    /// Upgrading to TLS is accepted from any transport: the phone then stops
    /// sending the bearer in plaintext from the next request on. Downgrading
    /// (clearing TLS) is accepted only from an `authenticated` source — an
    /// E2E Relay bootstrap or a pinned-TLS bootstrap — so a plaintext reply
    /// from a LAN impostor can never strip the pin from a Host that has one.
    static func applying(
        _ decision: RemoteDirectTransportDecision,
        to record: PairedMacRecord,
        authenticated: Bool
    ) -> PairedMacRecord? {
        var updated = record
        switch decision {
        case .tls(let fingerprint):
            updated.directTLSFingerprint = fingerprint
        case .plaintext:
            guard authenticated, record.directTLSFingerprint != nil else { return nil }
            updated.directTLSFingerprint = nil
        case .tlsUnpinnable, .unknown:
            return nil
        }
        return updated == record ? nil : updated
    }

    /// Whether a plaintext `/mobile` reply is the Host refusing the bearer
    /// over plaintext (the transition-era `426 Upgrade Required`, or a `401`
    /// whose message points at HTTPS/TLS). Any other 4xx keeps its meaning.
    static func isPlaintextRefusal(statusCode: Int, serverMessage: String?) -> Bool {
        if statusCode == 426 { return true }
        guard statusCode == 401, let serverMessage else { return false }
        let lowered = serverMessage.lowercased()
        return lowered.contains("https") || lowered.contains("tls")
    }

    /// Persisted `/mobile` endpoints are always spelled `http://` — the pin,
    /// not the stored scheme, decides the wire. Normalizing here keeps the
    /// endpoint-equality checks in the generation guards stable across a
    /// Host that starts advertising `https://`.
    static func canonicalStoredEndpoint(_ endpoint: URL) -> URL {
        guard endpoint.scheme?.lowercased() == "https",
              var components = URLComponents(url: endpoint, resolvingAgainstBaseURL: false)
        else { return endpoint }
        components.scheme = "http"
        return components.url ?? endpoint
    }
}

/// Bootstrap is the connection health signal, so its deadline is short on
/// the LAN. Over the relay a bootstrap crosses the tunnel twice plus the
/// Host's own work; on cellular that legitimately misses 4 s, and each miss
/// used to be read as "connection lost". Budget it from the measured path.
enum RemoteBootstrapDeadline {
    static let direct: TimeInterval = 4
    static let relayMinimum: TimeInterval = 10
    static let relayMaximum: TimeInterval = 20
    /// Multiplier on the last measured relay round-trip. A bootstrap is one
    /// request; giving it several RTTs of headroom absorbs jitter without
    /// letting a genuinely dead path linger past the keepalive limit.
    static let relayRoundTripMultiplier: Double = 5

    static func seconds(isRelay: Bool, measuredRoundTrip: TimeInterval?) -> TimeInterval {
        guard isRelay else { return direct }
        guard let measuredRoundTrip, measuredRoundTrip.isFinite, measuredRoundTrip > 0 else {
            return relayMinimum
        }
        let scaled = measuredRoundTrip * relayRoundTripMultiplier
        return min(relayMaximum, max(relayMinimum, scaled))
    }
}

/// How an APNs token reaches one paired Mac.
enum PushTokenRegistrationRoute: Equatable, Sendable {
    /// POST over the paired Direct endpoint (pinned HTTPS or legacy HTTP).
    case direct
    /// POST through the connection store's live Link connection for the
    /// active Mac — no second socket, no LAN wait.
    case activeRelayClient
    /// POST through a short-lived Link connection built for this Mac.
    case transientRelay

    /// Order of attempts for one Mac. The active Mac already on the relay
    /// skips the LAN entirely: that attempt is known to fail and used to hold
    /// the registration for the full 10 s POST timeout before opening a
    /// throwaway relay socket next to the live one.
    static func plan(
        isActiveMac: Bool,
        usingRelay: Bool,
        hasRelayCredentials: Bool
    ) -> [PushTokenRegistrationRoute] {
        if isActiveMac, usingRelay {
            return [.activeRelayClient]
        }
        var routes: [PushTokenRegistrationRoute] = [.direct]
        if hasRelayCredentials {
            routes.append(.transientRelay)
        }
        return routes
    }
}

/// One pinned URLSession per certificate fingerprint. Sharing the session
/// keeps TLS session resumption and HTTP keep-alive across the 2 s poll,
/// where a per-request session would pay a full handshake every time.
final class RemotePinnedURLSessionCache: @unchecked Sendable {
    static let shared = RemotePinnedURLSessionCache()

    private let lock = NSLock()
    private var sessions: [String: URLSession] = [:]

    func session(forFingerprint fingerprint: String) -> URLSession {
        let key = fingerprint.lowercased()
        lock.lock()
        defer { lock.unlock() }
        if let existing = sessions[key] { return existing }
        let configuration = URLSessionConfiguration.ephemeral
        configuration.waitsForConnectivity = false
        // Per-request `timeoutInterval` bounds every call; this only caps a
        // request that never set one.
        configuration.timeoutIntervalForRequest = 30
        let session = URLSession(
            configuration: configuration,
            delegate: RemoteCertificatePinningDelegate(pinnedFingerprint: key),
            delegateQueue: nil
        )
        sessions[key] = session
        return session
    }
}
