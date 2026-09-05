import Foundation

/// Persisted, transport-neutral identity for a Host paired to a Controller.
///
/// The shipped mobile-v1 wire still calls this identity `macID`. Keep that
/// compatibility name at the DTO boundary; app and TUI Controller state use
/// `hostID` because the Host may be a headless Mac or Linux machine.
public struct PairedHostRecord: Codable, Equatable, Identifiable, Sendable {
    public let hostID: String
    public var name: String
    public var endpoint: URL
    public let controllerDeviceID: String
    public let pairedAtUnixMs: Int64
    public var certificateFingerprint: String?
    public var remoteServerPort: Int?
    public var remoteServerCertificateFingerprint: String?
    /// Per-Host Unpeel Link scope, same narrows-only convention as the
    /// inbound device flag (`MobilePairedDeviceRecord.relayAllowed`): nil
    /// means allowed, and only `false` is ever stored, so records persisted
    /// by older builds (missing key) keep today's behavior. When false the
    /// Controller never opens the Link relay downlink for this Host — a
    /// Direct reachability failure stays a failure instead of falling back.
    public var linkEnabled: Bool?

    public var id: String { hostID }

    public var isLinkEnabled: Bool { linkEnabled ?? true }

    public init(
        hostID: String,
        name: String,
        endpoint: URL,
        controllerDeviceID: String,
        pairedAtUnixMs: Int64,
        certificateFingerprint: String? = nil,
        remoteServerPort: Int? = nil,
        remoteServerCertificateFingerprint: String? = nil,
        linkEnabled: Bool? = nil
    ) {
        self.hostID = hostID
        self.name = name
        self.endpoint = endpoint
        self.controllerDeviceID = controllerDeviceID
        self.pairedAtUnixMs = pairedAtUnixMs
        self.certificateFingerprint = certificateFingerprint
        self.remoteServerPort = remoteServerPort
        self.remoteServerCertificateFingerprint = remoteServerCertificateFingerprint
        self.linkEnabled = linkEnabled
    }

    /// Translate the compatibility DTO once, at the wire boundary.
    public init(pairing response: RemotePairingResponse, certificateFingerprint: String? = nil) {
        self.init(
            hostID: response.macID,
            name: response.macName,
            endpoint: response.directEndpoint ?? response.endpoint,
            controllerDeviceID: response.deviceID,
            pairedAtUnixMs: response.pairedAtUnixMs,
            certificateFingerprint: certificateFingerprint,
            remoteServerPort: response.remoteServerPort,
            remoteServerCertificateFingerprint: response.remoteServerCertificateFingerprint
        )
    }
}

public enum PairedHostCollection {
    /// Replace a Host in place so picker ordering is stable, or append a new
    /// Host in pairing order.
    public static func upserting(
        _ records: [PairedHostRecord],
        with record: PairedHostRecord
    ) -> [PairedHostRecord] {
        var result = records
        if let index = result.firstIndex(where: { $0.hostID == record.hostID }) {
            result[index] = record
        } else {
            result.append(record)
        }
        return result
    }

    public static func removing(
        _ records: [PairedHostRecord],
        hostID: String
    ) -> [PairedHostRecord] {
        records.filter { $0.hostID != hostID }
    }
}
