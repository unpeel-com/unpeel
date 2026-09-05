//
//  RemoteHosts.swift
//  UnpeelNative
//
//  Controller-side Host records. Public metadata lives in the isolated app
//  defaults suite; command-execution credentials stay in Keychain. Nothing
//  here reads or writes local Session state.
//

import AppKit
import Foundation
import Security
import UnpeelShared

/// The App→Host picker (Share This Mac…, Add Workspace… ▸ Nearby/code and
/// SSH, paired/SSH switcher rows). Released to everyone in 0.4.0 (decided
/// 2026-09-02 — early users, risks accepted) behind the Settings ▸
/// Experimental "Remote workspaces" switch, default on. Known caveats stay
/// documented rather than gating: Direct is bearer-authenticated plaintext
/// for LAN/VPN use (pinned Direct TLS is still unbuilt) and Link carries the
/// encrypted path off-network. Development bundles always show the picker.
enum RemoteHostFeature {
    static var pickerEnabled: Bool {
        if Bundle.main.object(forInfoDictionaryKey: "UnpeelDevelopmentBuild") as? Bool == true {
            return true
        }
        return UnpeelFeatureFlags.isEnabled(.remoteWorkspaces)
    }
}

struct RemoteHostCredentials: Codable, Equatable, Sendable {
    let authToken: String
    let relayCredentials: RelayCredentials
}

struct SSHHostRecord: Codable, Equatable, Identifiable, Sendable {
    let id: String
    var name: String
    var target: String
    var hostID: String
    var mode: RemoteSSHConnectionMode
    var usesStoredSecret: Bool

    var destination: String {
        String(target.dropFirst("ssh://".count))
    }
}

protocol SSHHostSecretStoring {
    func save(_ secret: String, account: String) throws
    func load(account: String) -> String?
    func delete(account: String)
}

struct KeychainSSHHostSecretStore: SSHHostSecretStoring {
    private static let service = "com.unpeel.native.ssh-host"

    private func query(account: String) -> [String: Any] {
        [
            kSecClass as String: kSecClassGenericPassword,
            kSecAttrService as String: Self.service,
            kSecAttrAccount as String: account,
        ]
    }

    func save(_ secret: String, account: String) throws {
        guard let data = secret.data(using: .utf8) else {
            throw RemoteHostCredentialError.encoding
        }
        let base = query(account: account)
        let updateStatus = SecItemUpdate(
            base as CFDictionary,
            [kSecValueData as String: data] as CFDictionary
        )
        if updateStatus == errSecSuccess { return }
        guard updateStatus == errSecItemNotFound else {
            throw RemoteHostCredentialError.keychain(updateStatus)
        }
        var insert = base
        insert[kSecValueData as String] = data
        insert[kSecAttrAccessible as String] = kSecAttrAccessibleWhenUnlockedThisDeviceOnly
        let status = SecItemAdd(insert as CFDictionary, nil)
        guard status == errSecSuccess else {
            throw RemoteHostCredentialError.keychain(status)
        }
    }

    func load(account: String) -> String? {
        var lookup = query(account: account)
        lookup[kSecReturnData as String] = true
        lookup[kSecMatchLimit as String] = kSecMatchLimitOne
        var result: AnyObject?
        guard SecItemCopyMatching(lookup as CFDictionary, &result) == errSecSuccess,
              let data = result as? Data
        else { return nil }
        return String(data: data, encoding: .utf8)
    }

    func delete(account: String) {
        SecItemDelete(query(account: account) as CFDictionary)
    }
}

protocol RemoteHostCredentialStoring {
    func save(_ credentials: RemoteHostCredentials, account: String) throws
    func load(account: String) -> RemoteHostCredentials?
    func delete(account: String)
}

enum RemoteHostCredentialError: LocalizedError {
    case keychain(OSStatus)
    case encoding

    var errorDescription: String? {
        switch self {
        case .keychain(let status):
            "Could not store the Host credential in Keychain (\(status))."
        case .encoding:
            "Could not encode the Host credential."
        }
    }
}

enum RemoteHostPairingError: LocalizedError {
    case invalidCode
    case incompatibleProtocol
    case candidateMismatch
    case selfPairing
    case expired
    case host(Int, String?)
    case authentication

    var errorDescription: String? {
        switch self {
        case .invalidCode:
            "That is not an Unpeel pairing code."
        case .incompatibleProtocol:
            "This Host uses an incompatible pairing protocol."
        case .candidateMismatch:
            "That code belongs to a different Host."
        case .selfPairing:
            "This is this Mac. Choose Local instead."
        case .expired:
            "That pairing code expired. Generate a new code on the Host."
        case .host(_, let message):
            message ?? "The Host rejected the pairing request."
        case .authentication:
            "The Host pairing response could not be authenticated."
        }
    }
}

enum SSHHostSetupError: LocalizedError {
    case invalidTarget
    case missingIdentity
    case connection(standard: String, interactive: String)
    case installation(standard: String, interactive: String)

    var errorDescription: String? {
        switch self {
        case .invalidTarget:
            "Enter an SSH config alias or user@host. Put ports, keys, and ProxyJump settings in ~/.ssh/config."
        case .missingIdentity:
            "The remote Unpeel Host did not provide a stable identity. Update Unpeel on the Host and try again."
        case let .connection(standard, interactive):
            "Could not start Unpeel over SSH. Standard SSH: \(standard) Interactive shell: \(interactive)"
        case let .installation(standard, interactive):
            "Could not install Unpeel over SSH. Standard SSH: \(standard) Interactive shell: \(interactive)"
        }
    }
}

struct KeychainRemoteHostCredentialStore: RemoteHostCredentialStoring {
    private static let service = "com.unpeel.native.remote-host"

    private func query(account: String) -> [String: Any] {
        [
            kSecClass as String: kSecClassGenericPassword,
            kSecAttrService as String: Self.service,
            kSecAttrAccount as String: account,
        ]
    }

    func save(_ credentials: RemoteHostCredentials, account: String) throws {
        guard let data = try? JSONEncoder().encode(credentials) else {
            throw RemoteHostCredentialError.encoding
        }
        let base = query(account: account)
        let updateStatus = SecItemUpdate(
            base as CFDictionary,
            [kSecValueData as String: data] as CFDictionary
        )
        if updateStatus == errSecSuccess { return }
        guard updateStatus == errSecItemNotFound else {
            throw RemoteHostCredentialError.keychain(updateStatus)
        }
        var insert = base
        insert[kSecValueData as String] = data
        insert[kSecAttrAccessible as String] = kSecAttrAccessibleWhenUnlockedThisDeviceOnly
        let status = SecItemAdd(insert as CFDictionary, nil)
        guard status == errSecSuccess else {
            throw RemoteHostCredentialError.keychain(status)
        }
    }

    func load(account: String) -> RemoteHostCredentials? {
        var lookup = query(account: account)
        lookup[kSecReturnData as String] = true
        lookup[kSecMatchLimit as String] = kSecMatchLimitOne
        var result: AnyObject?
        guard SecItemCopyMatching(lookup as CFDictionary, &result) == errSecSuccess,
              let data = result as? Data
        else { return nil }
        return try? JSONDecoder().decode(RemoteHostCredentials.self, from: data)
    }

    func delete(account: String) {
        SecItemDelete(query(account: account) as CFDictionary)
    }
}

@MainActor
final class RemoteHostStore: ObservableObject {
    private static let recordsKey = "unpeel.native.remoteHosts"
    private static let sshRecordsKey = "unpeel.native.sshHosts"
    private static let selectedHostKey = "unpeel.native.selectedRemoteHost"
    private static let controllerIDKey = "unpeel.native.remoteControllerID"

    @Published private(set) var records: [PairedHostRecord]
    @Published private(set) var sshRecords: [SSHHostRecord]
    /// Nil means Local. Fresh installs therefore always start in Local scope.
    @Published private(set) var selectedHostID: String?

    let controllerIdentity: RemoteDeviceIdentity

    private let defaults: UserDefaults
    private let credentialStore: any RemoteHostCredentialStoring
    private let sshSecretStore: any SSHHostSecretStoring
    private let localHostID: String?

    init(
        defaults: UserDefaults = AppDefaults.shared,
        credentialStore: any RemoteHostCredentialStoring = KeychainRemoteHostCredentialStore(),
        sshSecretStore: any SSHHostSecretStoring = KeychainSSHHostSecretStore(),
        localHostID: String? = nil,
        deviceName: String? = nil,
        appVersion: String? = Bundle.main.object(
            forInfoDictionaryKey: "CFBundleShortVersionString"
        ) as? String
    ) {
        self.defaults = defaults
        self.credentialStore = credentialStore
        self.sshSecretStore = sshSecretStore
        self.localHostID = localHostID
        let controllerID = Self.controllerID(in: defaults)
        controllerIdentity = RemoteDeviceIdentity(
            id: controllerID,
            name: deviceName
                ?? Host.current().localizedName
                ?? ProcessInfo.processInfo.hostName,
            platform: "macOS",
            appVersion: appVersion
        )
        let loadedRecords = Self.loadRecords(from: defaults)
        let removedLocalRecords = loadedRecords.filter {
            Self.hostIDsMatch($0.hostID, localHostID)
        }
        records = loadedRecords.filter {
            !Self.hostIDsMatch($0.hostID, localHostID)
        }
        let loadedSSHRecords = Self.loadSSHRecords(from: defaults)
        let removedLocalSSHRecords = loadedSSHRecords.filter {
            Self.hostIDsMatch($0.hostID, localHostID)
        }
        sshRecords = loadedSSHRecords.filter {
            !Self.hostIDsMatch($0.hostID, localHostID)
        }
        let persistedSelection = defaults.string(forKey: Self.selectedHostKey)
        if let persistedSelection,
           Self.selectionIsUsable(
               persistedSelection,
               pairedRecords: records,
               sshRecords: sshRecords,
               controllerID: controllerID,
               credentialStore: credentialStore,
               sshSecretStore: sshSecretStore
           ) {
            selectedHostID = persistedSelection
        } else {
            selectedHostID = nil
        }
        if selectedHostID == nil {
            defaults.removeObject(forKey: Self.selectedHostKey)
        }
        if !removedLocalRecords.isEmpty {
            for record in removedLocalRecords {
                credentialStore.delete(
                    account: Self.credentialAccount(
                        controllerID: controllerID,
                        hostID: record.hostID
                    )
                )
            }
            persistRecords()
        }
        if !removedLocalSSHRecords.isEmpty {
            for record in removedLocalSSHRecords {
                sshSecretStore.delete(account: Self.sshSecretAccount(
                    controllerID: controllerID,
                    recordID: record.id
                ))
            }
            persistSSHRecords()
        }
    }

    var selectedRecord: PairedHostRecord? {
        guard let selectedHostID else { return nil }
        return records.first { $0.hostID == selectedHostID }
    }

    var selectedSSHRecord: SSHHostRecord? {
        guard let selectedHostID else { return nil }
        return sshRecords.first { $0.id == selectedHostID }
    }

    var selectedDisplayName: String? {
        selectedRecord?.name ?? selectedSSHRecord?.name
    }

    func credentials(for hostID: String) -> RemoteHostCredentials? {
        credentialStore.load(account: credentialAccount(hostID: hostID))
    }

    func sshSecret(for recordID: String) -> String? {
        sshSecretStore.load(account: sshSecretAccount(recordID: recordID))
    }

    @discardableResult
    func adoptSSH(
        target: String,
        name: String,
        hostID: String,
        mode: RemoteSSHConnectionMode,
        secret: String?,
        select: Bool = true
    ) throws -> SSHHostRecord {
        guard !isLocalHost(hostID) else {
            throw RemoteHostPairingError.selfPairing
        }
        let existing = sshRecords.first { $0.target == target }
        let recordID = existing?.id ?? "ssh.\(UUID().uuidString.lowercased())"
        let normalizedSecret = secret.flatMap { $0.isEmpty ? nil : $0 }
        if let normalizedSecret {
            try sshSecretStore.save(
                normalizedSecret,
                account: sshSecretAccount(recordID: recordID)
            )
        } else {
            sshSecretStore.delete(account: sshSecretAccount(recordID: recordID))
        }
        let record = SSHHostRecord(
            id: recordID,
            name: name.isEmpty ? String(target.dropFirst("ssh://".count)) : name,
            target: target,
            hostID: hostID,
            mode: mode,
            usesStoredSecret: normalizedSecret != nil
        )
        if let index = sshRecords.firstIndex(where: { $0.id == recordID }) {
            sshRecords[index] = record
        } else {
            sshRecords.append(record)
        }
        persistSSHRecords()
        if select { selectHost(record.id) }
        return record
    }

    /// Complete the same sealed one-time handshake used by the iPhone. A
    /// nearby Bonjour row is only a convenience pre-filter: the code still
    /// authenticates the Host and an optional selected row must match it.
    @discardableResult
    func pair(
        code: String,
        expectedHostID: String? = nil,
        client: RemotePairingClient = RemotePairingClient()
    ) async throws -> PairedHostRecord {
        guard let payload = RemotePairingCode.decode(code) else {
            throw RemoteHostPairingError.invalidCode
        }
        guard payload.protocolVersion == RemoteControlProtocol.version else {
            throw RemoteHostPairingError.incompatibleProtocol
        }
        guard !isLocalHost(payload.macID) else {
            throw RemoteHostPairingError.selfPairing
        }
        if let expectedHostID,
           expectedHostID.caseInsensitiveCompare(payload.macID) != .orderedSame {
            throw RemoteHostPairingError.candidateMismatch
        }
        let response: RemotePairingResponse
        do {
            response = try await client.pair(
                payload: payload,
                device: controllerIdentity
            )
        } catch let error as RemotePairingClientError {
            switch error {
            case .expired:
                throw RemoteHostPairingError.expired
            case .incompatibleProtocol:
                throw RemoteHostPairingError.incompatibleProtocol
            case let .httpStatus(statusCode, serverMessage):
                throw RemoteHostPairingError.host(statusCode, serverMessage)
            case .invalidHostIdentity,
                 .invalidHTTPResponse,
                 .responseHostIdentityMismatch,
                 .responseEndpointMismatch,
                 .responseDeviceIdentityMismatch,
                 .invalidCredentials:
                throw RemoteHostPairingError.authentication
            }
        }
        try Task.checkCancellation()
        return try adopt(
            response,
            certificateFingerprint: payload.certificateFingerprint,
            select: false
        )
    }

    /// Persist a successfully authenticated pairing response. Metadata is
    /// committed only after Keychain accepts the secret, so a crash cannot
    /// leave a picker row that can never connect.
    @discardableResult
    func adopt(
        _ response: RemotePairingResponse,
        certificateFingerprint: String? = nil,
        select: Bool = true
    ) throws -> PairedHostRecord {
        guard response.protocolVersion == RemoteControlProtocol.version else {
            throw RemoteHostPairingError.incompatibleProtocol
        }
        guard !isLocalHost(response.macID) else {
            throw RemoteHostPairingError.selfPairing
        }
        guard !response.macID.isEmpty,
              response.deviceID == controllerIdentity.id,
              !response.authToken.isEmpty,
              response.relayCredentials.macID == response.macID,
              !response.relayCredentials.relayToken.isEmpty,
              response.relayCredentials.relayURL.scheme?.lowercased() == "wss",
              response.relayCredentials.e2eKey?.count == 32
        else {
            throw RemoteHostPairingError.authentication
        }
        let credentials = RemoteHostCredentials(
            authToken: response.authToken,
            relayCredentials: response.relayCredentials
        )
        try credentialStore.save(
            credentials,
            account: credentialAccount(hostID: response.macID)
        )
        let record = PairedHostRecord(
            pairing: response,
            certificateFingerprint: certificateFingerprint
        )
        records = PairedHostCollection.upserting(records, with: record)
        persistRecords()
        if select { selectHost(record.hostID) }
        return record
    }

    func selectHost(_ hostID: String?) {
        guard let hostID else {
            selectedHostID = nil
            defaults.removeObject(forKey: Self.selectedHostKey)
            return
        }
        let pairedIsUsable = records.contains(where: { $0.hostID == hostID })
            && credentials(for: hostID) != nil
        let sshIsUsable = sshRecords.first(where: { $0.id == hostID }).map {
            !$0.usesStoredSecret || sshSecret(for: $0.id) != nil
        } ?? false
        guard pairedIsUsable || sshIsUsable else { return }
        selectedHostID = hostID
        defaults.set(hostID, forKey: Self.selectedHostKey)
    }

    /// Scope a paired Host to Direct-only (enabled = false) or restore its
    /// Unpeel Link fallback. Same narrows-only storage convention as the
    /// inbound device flag: allowed is the nil default, so the persisted
    /// records gain no key until a Host is actually restricted.
    func setLinkEnabled(_ enabled: Bool, forHost hostID: String) {
        guard let index = records.firstIndex(where: { $0.hostID == hostID }),
              records[index].isLinkEnabled != enabled
        else { return }
        records[index].linkEnabled = enabled ? nil : false
        persistRecords()
    }

    /// Rename a Controller-side Host alias without changing its stable Host
    /// identity, transport target, or credentials. The Host may advertise a
    /// different machine name on the next bootstrap; the explicit alias wins
    /// in Controller UI until the user changes it again.
    @discardableResult
    func renameHost(_ hostID: String, to rawName: String) -> Bool {
        let name = rawName.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !name.isEmpty else { return false }
        if let index = records.firstIndex(where: { $0.hostID == hostID }) {
            guard records[index].name != name else { return true }
            records[index].name = name
            persistRecords()
            return true
        }
        if let index = sshRecords.firstIndex(where: { $0.id == hostID }) {
            guard sshRecords[index].name != name else { return true }
            sshRecords[index].name = name
            persistSSHRecords()
            return true
        }
        return false
    }

    func forget(hostID: String) {
        if let ssh = sshRecords.first(where: { $0.id == hostID }) {
            sshRecords.removeAll { $0.id == hostID }
            sshSecretStore.delete(account: sshSecretAccount(recordID: ssh.id))
            if selectedHostID == hostID { selectHost(nil) }
            persistSSHRecords()
            return
        }
        records = PairedHostCollection.removing(records, hostID: hostID)
        credentialStore.delete(account: credentialAccount(hostID: hostID))
        if selectedHostID == hostID { selectHost(nil) }
        persistRecords()
    }

    private func credentialAccount(hostID: String) -> String {
        Self.credentialAccount(controllerID: controllerIdentity.id, hostID: hostID)
    }

    private func sshSecretAccount(recordID: String) -> String {
        Self.sshSecretAccount(controllerID: controllerIdentity.id, recordID: recordID)
    }

    private func isLocalHost(_ hostID: String) -> Bool {
        Self.hostIDsMatch(hostID, localHostID)
    }

    private static func hostIDsMatch(_ hostID: String, _ otherHostID: String?) -> Bool {
        guard let otherHostID, !otherHostID.isEmpty else { return false }
        return otherHostID.caseInsensitiveCompare(hostID) == .orderedSame
    }

    private static func credentialAccount(controllerID: String, hostID: String) -> String {
        "pairing.\(controllerID).\(hostID)"
    }

    private static func sshSecretAccount(controllerID: String, recordID: String) -> String {
        "ssh.\(controllerID).\(recordID)"
    }

    private func persistRecords() {
        guard let data = try? JSONEncoder().encode(records) else { return }
        defaults.set(data, forKey: Self.recordsKey)
    }

    private func persistSSHRecords() {
        guard let data = try? JSONEncoder().encode(sshRecords) else { return }
        defaults.set(data, forKey: Self.sshRecordsKey)
    }

    private static func loadRecords(from defaults: UserDefaults) -> [PairedHostRecord] {
        guard let data = defaults.data(forKey: recordsKey),
              let records = try? JSONDecoder().decode([PairedHostRecord].self, from: data)
        else { return [] }
        var seen = Set<String>()
        return records.filter { !$0.hostID.isEmpty && seen.insert($0.hostID).inserted }
    }

    private static func loadSSHRecords(from defaults: UserDefaults) -> [SSHHostRecord] {
        guard let data = defaults.data(forKey: sshRecordsKey),
              let records = try? JSONDecoder().decode([SSHHostRecord].self, from: data)
        else { return [] }
        var seen = Set<String>()
        return records.filter {
            $0.id.hasPrefix("ssh.")
                && !$0.target.isEmpty
                && !$0.hostID.isEmpty
                && seen.insert($0.id).inserted
        }
    }

    private static func selectionIsUsable(
        _ selection: String,
        pairedRecords: [PairedHostRecord],
        sshRecords: [SSHHostRecord],
        controllerID: String,
        credentialStore: any RemoteHostCredentialStoring,
        sshSecretStore: any SSHHostSecretStoring
    ) -> Bool {
        if pairedRecords.contains(where: { $0.hostID == selection }) {
            return credentialStore.load(account: credentialAccount(
                controllerID: controllerID,
                hostID: selection
            )) != nil
        }
        guard let record = sshRecords.first(where: { $0.id == selection }) else {
            return false
        }
        return !record.usesStoredSecret || sshSecretStore.load(
            account: sshSecretAccount(controllerID: controllerID, recordID: record.id)
        ) != nil
    }

    private static func controllerID(in defaults: UserDefaults) -> String {
        if let value = defaults.string(forKey: controllerIDKey), !value.isEmpty {
            return value
        }
        let value = UUID().uuidString.lowercased()
        defaults.set(value, forKey: controllerIDKey)
        return value
    }
}
