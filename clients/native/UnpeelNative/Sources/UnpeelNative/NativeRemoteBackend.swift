//
//  NativeRemoteBackend.swift
//  UnpeelNative
//
//  Swift ownership boundary for the Rust RemoteSessionBackend. The Rust side
//  owns connection generations and SSH processes; Swift owns only an opaque
//  integer handle and typed snapshots. Every blocking bridge call runs away
//  from the main actor.
//

import CUnpeelNativeBridge
import Foundation
import UnpeelShared

enum RemoteSSHConnectionMode: String, Codable, CaseIterable, Sendable {
    case command
    case interactiveShell

    var displayName: String {
        switch self {
        case .command: "Standard SSH"
        case .interactiveShell: "Interactive shell"
        }
    }
}

struct NativeRemoteBackendError: Error, LocalizedError, Equatable, Sendable {
    let result: Int32
    let code: String
    let message: String
    let kind: String?
    let operation: String?

    init(
        result: Int32,
        code: String,
        message: String,
        kind: String? = nil,
        operation: String? = nil
    ) {
        self.result = result
        self.code = code
        self.message = message
        self.kind = kind
        self.operation = operation
    }

    var errorDescription: String? { message }

    /// An at-most-once effect may have reached the Host even though its
    /// receipt was lost. Callers must surface this state and never retry it.
    var effectOutcomeIsUnknown: Bool { kind == "outcomeUnknown" }

    /// A correlated Host response proved that the effect did not run. The
    /// caller may drop that one effect and continue later queued work on the
    /// same accepted connection generation.
    var effectWasNotApplied: Bool { kind == "notApplied" }

    /// Only a correlated semantic Host rejection proves both non-application
    /// and that the accepted generation remains callable. Transport-level
    /// NotSent failures are also `notApplied`, but Rust invalidates their
    /// generation; later queued effects must not reconnect behind the UI gate.
    var effectCanContinueOnCurrentGeneration: Bool {
        effectWasNotApplied && code == "host_operation_rejected"
    }
}

struct NativeRemoteOutputPageMetadata: Decodable, Equatable, Sendable {
    let sessionID: String
    let requestedOffset: UInt64?
    let offset: UInt64
    let nextOffset: UInt64
    let resetBeforeFeed: Bool
    let truncated: Bool
    let capturedAtUnixMs: Int64
    let byteCount: Int
}

final class NativeRemoteOutputPage: @unchecked Sendable {
    let metadata: NativeRemoteOutputPageMetadata
    let bytes: Data

    private let lock = NSLock()
    private var resolution: (
        parent: unpeel_native_bridge_remote_handle_t,
        page: unpeel_native_bridge_remote_output_page_handle_t
    )?

    init(
        metadata: NativeRemoteOutputPageMetadata,
        bytes: Data,
        parentHandle: unpeel_native_bridge_remote_handle_t,
        pageHandle: unpeel_native_bridge_remote_output_page_handle_t
    ) {
        self.metadata = metadata
        self.bytes = bytes
        resolution = (parentHandle, pageHandle)
    }

    /// Commit/discard share this one-shot claim. A reference-semantic lease
    /// cannot be copied into two independently resolvable page values.
    func claimResolution() -> (
        parent: unpeel_native_bridge_remote_handle_t,
        page: unpeel_native_bridge_remote_output_page_handle_t
    )? {
        lock.lock()
        defer { lock.unlock() }
        defer { resolution = nil }
        return resolution
    }

    deinit {
        guard let resolution = claimResolution() else { return }
        Task.detached(priority: .utility) {
            NativeRemoteBackend.discardIgnoringError(
                parentHandle: resolution.parent,
                pageHandle: resolution.page
            )
        }
    }
}

struct NativeRemoteEffectReceipt: Decodable, Equatable, Sendable {
    let requestID: UInt64
}

/// One live Session's current terminal grid from the Host's viewport
/// snapshot (`session.metrics.read`). `outputOffset` is nil on Hosts that
/// predate it in the gateway metrics body — the grid alone is what a
/// Controller's fit math needs.
struct NativeRemoteSessionMetrics: Decodable, Equatable, Sendable {
    let sessionID: String
    let columns: Int
    let rows: Int
    let outputOffset: UInt64?
    let capturedAtUnixMs: Int64
}

/// Receipt for a Controller-created Session. `session` is the optimistic
/// summary newer Hosts return; headless Hosts may omit it and let the next
/// bootstrap publish the row.
struct NativeRemoteCreatedSession: Decodable, Equatable, Sendable {
    let requestID: UInt64
    let sessionID: String
    let capturedAtUnixMs: Int64?
    let session: RemoteSessionSummary?
}

protocol NativeRemoteBackendProtocol: Sendable {
    func bootstrap() async throws -> RemoteBootstrapSnapshot
    func pollOutput(
        sessionID: String,
        limit: Int,
        waitMilliseconds: UInt64
    ) async throws -> NativeRemoteOutputPage
    func pollOutputFrom(
        sessionID: String,
        requestedOffset: UInt64?,
        limit: Int,
        waitMilliseconds: UInt64
    ) async throws -> NativeRemoteOutputPage
    func commitOutput(_ page: NativeRemoteOutputPage) async throws
    func discardOutput(_ page: NativeRemoteOutputPage) async
    func resetOutput(sessionID: String) async throws
    func writeTerminal(sessionID: String, data: Data) async throws -> NativeRemoteEffectReceipt
    func fitDesktop(
        sessionID: String,
        columns: UInt16,
        rows: UInt16
    ) async throws -> NativeRemoteEffectReceipt
    func clearDesktopFit(sessionID: String) async throws -> NativeRemoteEffectReceipt
    func markRead(sessionID: String) async throws -> NativeRemoteEffectReceipt
    func setSessionTitle(sessionID: String, title: String) async throws
        -> NativeRemoteEffectReceipt
    func setSessionPinned(sessionID: String, pinned: Bool) async throws
        -> NativeRemoteEffectReceipt
    func setSessionNotifyWhenDone(sessionID: String, enabled: Bool) async throws
        -> NativeRemoteEffectReceipt
    func answerApproval(id: String, approved: Bool) async throws
        -> NativeRemoteEffectReceipt
    /// File the Session under another project/group (`session.project.set`).
    func setSessionProject(sessionID: String, projectID: String) async throws
        -> NativeRemoteEffectReceipt
    func archiveSession(sessionID: String) async throws -> NativeRemoteEffectReceipt
    func restoreSession(sessionID: String) async throws -> NativeRemoteEffectReceipt
    func stopSession(sessionID: String) async throws -> NativeRemoteEffectReceipt
    func removeSession(sessionID: String) async throws -> NativeRemoteEffectReceipt
    func restartSession(sessionID: String) async throws -> NativeRemoteEffectReceipt
    func resumeAgent(sessionID: String) async throws -> NativeRemoteEffectReceipt
    func setSessionOrder(
        projectID: String,
        orderedSessionIDs: [String]
    ) async throws -> NativeRemoteEffectReceipt
    func setProjectOrganization(
        projectID: String,
        patch: RemoteProjectOrganizationPatch
    ) async throws -> NativeRemoteEffectReceipt
    func setPreset(patch: RemotePresetPatch) async throws -> NativeRemoteEffectReceipt
    func setWorkspaceSettings(patch: RemoteWorkspaceSettingsPatch) async throws -> NativeRemoteEffectReceipt
    func createSession(
        _ request: RemoteCreateSessionRequest
    ) async throws -> NativeRemoteCreatedSession
    func pairingInvitation(_ requestJSON: Data) async throws -> Data
    /// Upload image bytes to the Host (`artifact.upload`); returns the
    /// HOST-side path to paste as an attachable reference.
    func uploadAttachment(
        sessionID: String?,
        contentType: String,
        bytes: Data
    ) async throws -> String
    func listArchivedSessions(projectID: String) async throws -> [RemoteSessionSummary]
    func transcriptMarkdown(
        sessionID: String,
        entries: Int?
    ) async throws -> RemoteTranscriptMarkdown
    func sessionMetrics(sessionID: String) async throws -> NativeRemoteSessionMetrics
    func close() async
}

extension NativeRemoteBackendProtocol {
    /// Default for gateways and test doubles that predate the platform
    /// adapter verb: fail closed rather than mutating Controller-local state.
    func setSessionNotifyWhenDone(
        sessionID: String, enabled: Bool
    ) async throws -> NativeRemoteEffectReceipt {
        throw NativeRemoteBackendError(
            result: Int32(UNPEEL_NATIVE_BRIDGE_ERROR_REMOTE),
            code: "notify_when_done_unavailable",
            message: "Notify when done is unavailable on this backend.",
            kind: "notApplied",
            operation: "notify when done"
        )
    }

    func answerApproval(id: String, approved: Bool) async throws
        -> NativeRemoteEffectReceipt
    {
        throw NativeRemoteBackendError(
            result: Int32(UNPEEL_NATIVE_BRIDGE_ERROR_REMOTE),
            code: "approval_answer_unavailable",
            message: "Approval answering is unavailable on this backend.",
            kind: "notApplied",
            operation: "approval answer"
        )
    }

    /// Default for backends that predate attachment upload: an honest
    /// not-applied failure instead of a silent success.
    func uploadAttachment(
        sessionID: String?,
        contentType: String,
        bytes: Data
    ) async throws -> String {
        throw NativeRemoteBackendError(
            result: Int32(UNPEEL_NATIVE_BRIDGE_ERROR_REMOTE),
            code: "upload_unavailable",
            message: "Attachment upload is unavailable on this backend.",
            kind: "notApplied",
            operation: "attachment upload"
        )
    }

    /// Default for backends that predate session project moves.
    func setSessionProject(
        sessionID: String, projectID: String
    ) async throws -> NativeRemoteEffectReceipt {
        throw NativeRemoteBackendError(
            result: Int32(UNPEEL_NATIVE_BRIDGE_ERROR_REMOTE),
            code: "session_project_unavailable",
            message: "Session project moves are unavailable on this backend.",
            kind: "notApplied",
            operation: "session project"
        )
    }

    /// Default for gateways and test stubs that predate preset editing: an
    /// honest not-applied failure instead of a silent success.
    func setPreset(patch: RemotePresetPatch) async throws -> NativeRemoteEffectReceipt {
        throw NativeRemoteBackendError(
            result: Int32(UNPEEL_NATIVE_BRIDGE_ERROR_REMOTE),
            code: "preset_edit_unavailable",
            message: "Preset editing is unavailable on this backend.",
            kind: "notApplied",
            operation: "preset edit"
        )
    }

    func setWorkspaceSettings(
        patch: RemoteWorkspaceSettingsPatch
    ) async throws -> NativeRemoteEffectReceipt {
        throw NativeRemoteBackendError(
            result: Int32(UNPEEL_NATIVE_BRIDGE_ERROR_REMOTE),
            code: "workspace_settings_unavailable",
            message: "Workspace settings are unavailable on this backend.",
            kind: "notApplied",
            operation: "workspace settings"
        )
    }
}

/// One transport-backed remote Host connection. Calls may race with `close()`;
/// Rust keeps an in-flight backend alive long enough to fail safely, while
/// the higher-level Host runtime generation-gates late results before publish.
final class NativeRemoteBackend: @unchecked Sendable {
    private static let supportedABIVersion: UInt32 = 1
    /// Matches `remote_session_backend::MAX_OUTPUT_PAGE_BYTES`. Keeping the
    /// bound here prevents a default call that core must reject.
    static let maximumOutputPageBytes = 200 * 1024

    private enum OutputPollCursor: Sendable {
        case current
        case requested(UInt64?)
    }

    private let lock = NSLock()
    private let expectedHostID: String?
    private var handle: unpeel_native_bridge_remote_handle_t
    /// Output/effects are unavailable until Swift has decoded a bootstrap and
    /// checked its Host id. Rust may lazily bootstrap internally, so this gate
    /// is the saved-identity trust boundary, not merely call ordering.
    private var identityValidatedHandle: unpeel_native_bridge_remote_handle_t?

    /// Install the released Host binaries with the same system SSH policy and
    /// optional askpass credential used by a normal remote connection. Rust
    /// owns the fixed install command; Swift cannot pass arbitrary shell text.
    static func installUnpeel(
        sshTarget: String,
        mode: RemoteSSHConnectionMode,
        secret: String? = nil,
        askpassProgram: String = LaunchConfig.hostBinary
    ) async throws {
        struct SSHInstallConfig: Encodable {
            let target: String
            let mode: RemoteSSHConnectionMode
            let askpassProgram: String?
            let secret: String?
        }
        let normalizedSecret = secret.flatMap { $0.isEmpty ? nil : $0 }
        let config = try JSONEncoder().encode(SSHInstallConfig(
            target: sshTarget,
            mode: mode,
            askpassProgram: normalizedSecret == nil ? nil : askpassProgram,
            secret: normalizedSecret
        ))
        try await runBlocking(priority: .userInitiated) {
            try Task.checkCancellation()
            var outputPointer: UnsafeMutablePointer<UInt8>?
            var outputLength = 0
            let result = config.withUnsafeBytes { bytes in
                unpeel_native_bridge_remote_ssh_install(
                    bytes.bindMemory(to: UInt8.self).baseAddress,
                    bytes.count,
                    &outputPointer,
                    &outputLength
                )
            }
            let output = takeOutput(outputPointer, length: outputLength)
            try Task.checkCancellation()
            guard result == UNPEEL_NATIVE_BRIDGE_OK else {
                throw bridgeError(
                    result: result,
                    output: output,
                    fallbackCode: "ssh_install_failed",
                    fallbackMessage: "Could not install Unpeel on the SSH Host."
                )
            }
        }
    }

    init(
        sshTarget: String,
        expectedHostID: String? = nil,
        mode: RemoteSSHConnectionMode = .command,
        secret: String? = nil,
        askpassProgram: String = LaunchConfig.hostBinary
    ) throws {
        guard unpeel_native_bridge_abi_version() == Self.supportedABIVersion else {
            throw NativeRemoteBackendError(
                result: Int32(UNPEEL_NATIVE_BRIDGE_ERROR_INVALID_INPUT),
                code: "unsupported_bridge_abi",
                message: "This Unpeel build has an incompatible remote Host bridge."
            )
        }

        struct SSHOpenConfig: Encodable {
            let target: String
            let mode: RemoteSSHConnectionMode
            let askpassProgram: String?
            let secret: String?
        }
        let normalizedSecret = secret.flatMap { $0.isEmpty ? nil : $0 }
        let config: Data
        do {
            config = try JSONEncoder().encode(SSHOpenConfig(
                target: sshTarget,
                mode: mode,
                askpassProgram: normalizedSecret == nil ? nil : askpassProgram,
                secret: normalizedSecret
            ))
        } catch {
            throw NativeRemoteBackendError(
                result: Int32(UNPEEL_NATIVE_BRIDGE_ERROR_INVALID_INPUT),
                code: "invalid_ssh_config",
                message: "Could not prepare the SSH Host connection."
            )
        }
        var openedHandle: unpeel_native_bridge_remote_handle_t = 0
        var outputPointer: UnsafeMutablePointer<UInt8>?
        var outputLength = 0
        let result = config.withUnsafeBytes { bytes in
            unpeel_native_bridge_remote_ssh_config_open(
                bytes.bindMemory(to: UInt8.self).baseAddress,
                bytes.count,
                &openedHandle,
                &outputPointer,
                &outputLength
            )
        }
        let output = Self.takeOutput(outputPointer, length: outputLength)
        guard result == UNPEEL_NATIVE_BRIDGE_OK, openedHandle != 0 else {
            throw Self.bridgeError(
                result: result,
                output: output,
                fallbackCode: "remote_open_failed",
                fallbackMessage: "Could not open the remote Host."
            )
        }
        self.expectedHostID = expectedHostID
        handle = openedHandle
    }

    /// Open the loopback gateway to another LOCAL workspace: Rust spawns this
    /// app's bundled `unpeel-host __remote_stdio__` against the workspace's
    /// home on the first bootstrap. Same Host contract, generations, and
    /// effect certainty as SSH; only the child argv/env differs.
    init(
        localGatewayHome: String,
        expectedHostID: String?,
        requireHostService: Bool = false,
        hostProgram: String = LaunchConfig.hostBinary
    ) throws {
        guard unpeel_native_bridge_abi_version() == Self.supportedABIVersion else {
            throw NativeRemoteBackendError(
                result: Int32(UNPEEL_NATIVE_BRIDGE_ERROR_INVALID_INPUT),
                code: "unsupported_bridge_abi",
                message: "This Unpeel build has an incompatible remote Host bridge."
            )
        }

        struct LocalGatewayOpenConfig: Encodable {
            let hostProgram: String
            let unpeelHome: String
            let requireHostService: Bool
        }
        let config: Data
        do {
            config = try JSONEncoder().encode(LocalGatewayOpenConfig(
                hostProgram: hostProgram,
                unpeelHome: localGatewayHome,
                requireHostService: requireHostService
            ))
        } catch {
            throw NativeRemoteBackendError(
                result: Int32(UNPEEL_NATIVE_BRIDGE_ERROR_INVALID_INPUT),
                code: "invalid_local_gateway_config",
                message: "Could not prepare the workspace connection."
            )
        }
        var openedHandle: unpeel_native_bridge_remote_handle_t = 0
        var outputPointer: UnsafeMutablePointer<UInt8>?
        var outputLength = 0
        let result = config.withUnsafeBytes { bytes in
            unpeel_native_bridge_remote_local_gateway_open(
                bytes.bindMemory(to: UInt8.self).baseAddress,
                bytes.count,
                &openedHandle,
                &outputPointer,
                &outputLength
            )
        }
        let output = Self.takeOutput(outputPointer, length: outputLength)
        guard result == UNPEEL_NATIVE_BRIDGE_OK, openedHandle != 0 else {
            throw Self.bridgeError(
                result: result,
                output: output,
                fallbackCode: "remote_open_failed",
                fallbackMessage: "Could not open the workspace."
            )
        }
        self.expectedHostID = expectedHostID
        handle = openedHandle
    }

    /// Open the bearer-authenticated paired-LAN transport. The bridge parses
    /// the exact `http://…/mobile` scope now but performs no network I/O until
    /// `bootstrap()`. The bearer is passed only to Rust and is never included
    /// in metadata or errors.
    init(
        directEndpoint: URL,
        authToken: String,
        expectedHostID: String
    ) throws {
        guard unpeel_native_bridge_abi_version() == Self.supportedABIVersion else {
            throw NativeRemoteBackendError(
                result: Int32(UNPEEL_NATIVE_BRIDGE_ERROR_INVALID_INPUT),
                code: "unsupported_bridge_abi",
                message: "This Unpeel build has an incompatible remote Host bridge."
            )
        }

        let endpoint = Data(directEndpoint.absoluteString.utf8)
        let bearer = Data(authToken.utf8)
        var openedHandle: unpeel_native_bridge_remote_handle_t = 0
        var outputPointer: UnsafeMutablePointer<UInt8>?
        var outputLength = 0
        let result = endpoint.withUnsafeBytes { endpointBytes in
            bearer.withUnsafeBytes { bearerBytes in
                unpeel_native_bridge_remote_direct_open(
                    endpointBytes.bindMemory(to: UInt8.self).baseAddress,
                    endpointBytes.count,
                    bearerBytes.bindMemory(to: UInt8.self).baseAddress,
                    bearerBytes.count,
                    &openedHandle,
                    &outputPointer,
                    &outputLength
                )
            }
        }
        let output = Self.takeOutput(outputPointer, length: outputLength)
        guard result == UNPEEL_NATIVE_BRIDGE_OK, openedHandle != 0 else {
            throw Self.bridgeError(
                result: result,
                output: output,
                fallbackCode: "remote_direct_open_failed",
                fallbackMessage: "Could not open the paired Host."
            )
        }
        self.expectedHostID = expectedHostID
        handle = openedHandle
    }

    /// Open the canonical shared Swift Link downlink beneath the same Rust
    /// RemoteSessionBackend used by Direct and SSH. The first socket/handshake
    /// is lazy until bootstrap; expectedHostID is the durable paired-record
    /// trust anchor and is mandatory for every Link connection.
    init(
        relayCredentials: RelayCredentials,
        controllerDeviceID: String,
        authToken: String,
        expectedHostID: String
    ) throws {
        guard unpeel_native_bridge_abi_version() == Self.supportedABIVersion else {
            throw NativeRemoteBackendError(
                result: Int32(UNPEEL_NATIVE_BRIDGE_ERROR_INVALID_INPUT),
                code: "unsupported_bridge_abi",
                message: "This Unpeel build has an incompatible remote Host bridge."
            )
        }
        guard !expectedHostID.isEmpty,
              relayCredentials.macID == expectedHostID,
              !controllerDeviceID.isEmpty
        else {
            throw Self.invalidInput(
                code: "invalid_link_host_identity",
                message: "Unpeel Link credentials do not match the paired Host. Pair it again."
            )
        }
        handle = try NativeRelayBridge.open(
            credentials: relayCredentials,
            deviceID: controllerDeviceID,
            authToken: authToken
        )
        self.expectedHostID = expectedHostID
    }

    deinit {
        guard let handle = takeHandle() else { return }
        // A last-resort owner cleanup must not synchronously kill/wait an SSH
        // child on the main thread. Normal owners still call `close()` and
        // await it; this detached finalizer handles abandoned instances.
        Task.detached(priority: .utility) {
            Self.closeIgnoringError(handle)
        }
    }

    func bootstrap() async throws -> RemoteBootstrapSnapshot {
        let handle = try currentHandle()
        let expectedHostID = expectedHostID
        let snapshot = try await Self.runBlocking(priority: .userInitiated) {
            try Task.checkCancellation()
            var outputPointer: UnsafeMutablePointer<UInt8>?
            var outputLength = 0
            let result = unpeel_native_bridge_remote_bootstrap(
                handle,
                &outputPointer,
                &outputLength
            )
            let output = Self.takeOutput(outputPointer, length: outputLength)
            try Task.checkCancellation()
            guard result == UNPEEL_NATIVE_BRIDGE_OK else {
                throw Self.bridgeError(
                    result: result,
                    output: output,
                    fallbackCode: "remote_bootstrap_failed",
                    fallbackMessage: "Could not load the remote Host."
                )
            }
            let snapshot: RemoteBootstrapSnapshot
            do {
                snapshot = try JSONDecoder().decode(RemoteBootstrapSnapshot.self, from: output)
            } catch {
                throw NativeRemoteBackendError(
                    result: Int32(UNPEEL_NATIVE_BRIDGE_ERROR_SERIALIZATION),
                    code: "invalid_remote_bootstrap",
                    message: "The remote Host returned an invalid session snapshot."
                )
            }
            // Keep this outside the decode catch: a saved-id mismatch is a
            // repair-required trust failure, not malformed JSON.
            try Self.validateHostIdentity(snapshot, expectedHostID: expectedHostID)
            return snapshot
        }
        try markIdentityValidated(for: handle)
        return snapshot
    }

    func pollOutput(
        sessionID: String,
        limit: Int = NativeRemoteBackend.maximumOutputPageBytes,
        waitMilliseconds: UInt64 = 1_000
    ) async throws -> NativeRemoteOutputPage {
        try await pollOutput(
            sessionID: sessionID,
            cursor: .current,
            limit: limit,
            waitMilliseconds: waitMilliseconds
        )
    }

    /// Replace this backend's cursor with the Controller's exact rendered
    /// offset and reserve the replacement page atomically. A nil offset means
    /// an explicit fresh bounded-tail replay, not continuation from a prior
    /// renderer epoch.
    func pollOutputFrom(
        sessionID: String,
        requestedOffset: UInt64?,
        limit: Int = NativeRemoteBackend.maximumOutputPageBytes,
        waitMilliseconds: UInt64 = 1_000
    ) async throws -> NativeRemoteOutputPage {
        try await pollOutput(
            sessionID: sessionID,
            cursor: .requested(requestedOffset),
            limit: limit,
            waitMilliseconds: waitMilliseconds
        )
    }

    private func pollOutput(
        sessionID: String,
        cursor: OutputPollCursor,
        limit: Int,
        waitMilliseconds: UInt64
    ) async throws -> NativeRemoteOutputPage {
        guard (1...Self.maximumOutputPageBytes).contains(limit) else {
            throw Self.invalidInput(
                code: "invalid_output_limit",
                message: "Remote output limit must be between 1 and \(Self.maximumOutputPageBytes) bytes."
            )
        }
        let handle = try currentIdentityValidatedHandle()
        let session = Data(sessionID.utf8)
        return try await Self.runBlocking(priority: .userInitiated) {
            try Task.checkCancellation()
            var pageHandle: unpeel_native_bridge_remote_output_page_handle_t = 0
            var metadataPointer: UnsafeMutablePointer<UInt8>?
            var metadataLength = 0
            var bytesPointer: UnsafeMutablePointer<UInt8>?
            var bytesLength = 0
            let result = session.withUnsafeBytes { sessionBytes in
                switch cursor {
                case .current:
                    unpeel_native_bridge_remote_output_poll(
                        handle,
                        sessionBytes.bindMemory(to: UInt8.self).baseAddress,
                        sessionBytes.count,
                        limit,
                        waitMilliseconds,
                        &pageHandle,
                        &metadataPointer,
                        &metadataLength,
                        &bytesPointer,
                        &bytesLength
                    )
                case let .requested(requestedOffset):
                    unpeel_native_bridge_remote_output_poll_from(
                        handle,
                        sessionBytes.bindMemory(to: UInt8.self).baseAddress,
                        sessionBytes.count,
                        requestedOffset ?? 0,
                        requestedOffset == nil ? 0 : 1,
                        limit,
                        waitMilliseconds,
                        &pageHandle,
                        &metadataPointer,
                        &metadataLength,
                        &bytesPointer,
                        &bytesLength
                    )
                }
            }
            let metadataData = Self.takeOutput(metadataPointer, length: metadataLength)
            let bytes = Self.takeOutput(bytesPointer, length: bytesLength)
            guard result == UNPEEL_NATIVE_BRIDGE_OK, pageHandle != 0 else {
                throw Self.bridgeError(
                    result: result,
                    output: metadataData,
                    fallbackCode: "remote_output_failed",
                    fallbackMessage: "Could not read the remote terminal."
                )
            }

            if Task.isCancelled {
                Self.discardIgnoringError(parentHandle: handle, pageHandle: pageHandle)
                throw CancellationError()
            }

            let metadata: NativeRemoteOutputPageMetadata
            do {
                metadata = try JSONDecoder().decode(
                    NativeRemoteOutputPageMetadata.self,
                    from: metadataData
                )
            } catch {
                Self.discardIgnoringError(parentHandle: handle, pageHandle: pageHandle)
                throw NativeRemoteBackendError(
                    result: Int32(UNPEEL_NATIVE_BRIDGE_ERROR_SERIALIZATION),
                    code: "invalid_remote_output_metadata",
                    message: "The remote Host returned invalid terminal output metadata."
                )
            }
            guard metadata.sessionID == sessionID,
                  metadata.byteCount == bytes.count
            else {
                Self.discardIgnoringError(parentHandle: handle, pageHandle: pageHandle)
                throw NativeRemoteBackendError(
                    result: Int32(UNPEEL_NATIVE_BRIDGE_ERROR_SERIALIZATION),
                    code: "remote_output_metadata_mismatch",
                    message: "The remote Host returned terminal bytes for the wrong Session."
                )
            }
            return NativeRemoteOutputPage(
                metadata: metadata,
                bytes: bytes,
                parentHandle: handle,
                pageHandle: pageHandle
            )
        }
    }

    func commitOutput(_ page: NativeRemoteOutputPage) async throws {
        let handle = try currentHandle()
        guard let resolution = page.claimResolution() else {
            throw Self.invalidInput(
                code: "remote_output_page_already_resolved",
                message: "That terminal output page was already committed or discarded."
            )
        }
        guard resolution.parent == handle else {
            Self.discardIgnoringError(
                parentHandle: resolution.parent,
                pageHandle: resolution.page
            )
            throw Self.invalidInput(
                code: "wrong_remote_output_page_parent",
                message: "That terminal output page belongs to another Host connection."
            )
        }
        try await Self.runBlocking(priority: .userInitiated) {
            var outputPointer: UnsafeMutablePointer<UInt8>?
            var outputLength = 0
            let result = unpeel_native_bridge_remote_output_commit(
                handle,
                resolution.page,
                &outputPointer,
                &outputLength
            )
            let output = Self.takeOutput(outputPointer, length: outputLength)
            guard result == UNPEEL_NATIVE_BRIDGE_OK else {
                throw Self.bridgeError(
                    result: result,
                    output: output,
                    fallbackCode: "remote_output_commit_failed",
                    fallbackMessage: "Could not commit remote terminal output."
                )
            }
        }
    }

    func discardOutput(_ page: NativeRemoteOutputPage) async {
        guard let resolution = page.claimResolution() else { return }
        await Task.detached(priority: .utility) {
            Self.discardIgnoringError(
                parentHandle: resolution.parent,
                pageHandle: resolution.page
            )
        }.value
    }

    func resetOutput(sessionID: String) async throws {
        let handle = try currentIdentityValidatedHandle()
        let session = Data(sessionID.utf8)
        try await Self.runBlocking(priority: .userInitiated) {
            try Task.checkCancellation()
            var outputPointer: UnsafeMutablePointer<UInt8>?
            var outputLength = 0
            let result = session.withUnsafeBytes { bytes in
                unpeel_native_bridge_remote_output_reset(
                    handle,
                    bytes.bindMemory(to: UInt8.self).baseAddress,
                    bytes.count,
                    &outputPointer,
                    &outputLength
                )
            }
            let output = Self.takeOutput(outputPointer, length: outputLength)
            guard result == UNPEEL_NATIVE_BRIDGE_OK else {
                throw Self.bridgeError(
                    result: result,
                    output: output,
                    fallbackCode: "remote_output_reset_failed",
                    fallbackMessage: "Could not reset the remote terminal output cursor."
                )
            }
        }
    }

    func writeTerminal(
        sessionID: String,
        data: Data
    ) async throws -> NativeRemoteEffectReceipt {
        let handle = try currentIdentityValidatedHandle()
        let session = Data(sessionID.utf8)
        return try await Self.runBlocking(priority: .userInitiated) {
            // Cancellation before dispatch is safely not-applied. Never check
            // again after the FFI call: a landed effect's receipt must win.
            try Task.checkCancellation()
            var outputPointer: UnsafeMutablePointer<UInt8>?
            var outputLength = 0
            let result = session.withUnsafeBytes { sessionBytes in
                data.withUnsafeBytes { dataBytes in
                    unpeel_native_bridge_remote_terminal_write(
                        handle,
                        sessionBytes.bindMemory(to: UInt8.self).baseAddress,
                        sessionBytes.count,
                        dataBytes.bindMemory(to: UInt8.self).baseAddress,
                        dataBytes.count,
                        &outputPointer,
                        &outputLength
                    )
                }
            }
            return try Self.decodeEffect(
                result: result,
                pointer: outputPointer,
                length: outputLength,
                operation: "terminal write"
            )
        }
    }

    func fitDesktop(
        sessionID: String,
        columns: UInt16,
        rows: UInt16
    ) async throws -> NativeRemoteEffectReceipt {
        let handle = try currentIdentityValidatedHandle()
        let session = Data(sessionID.utf8)
        return try await Self.runBlocking(priority: .userInitiated) {
            try Task.checkCancellation()
            var outputPointer: UnsafeMutablePointer<UInt8>?
            var outputLength = 0
            let result = session.withUnsafeBytes { bytes in
                unpeel_native_bridge_remote_desktop_fit(
                    handle,
                    bytes.bindMemory(to: UInt8.self).baseAddress,
                    bytes.count,
                    columns,
                    rows,
                    &outputPointer,
                    &outputLength
                )
            }
            return try Self.decodeEffect(
                result: result,
                pointer: outputPointer,
                length: outputLength,
                operation: "desktop resize"
            )
        }
    }

    func clearDesktopFit(sessionID: String) async throws -> NativeRemoteEffectReceipt {
        let handle = try currentIdentityValidatedHandle()
        let session = Data(sessionID.utf8)
        return try await Self.runBlocking(priority: .userInitiated) {
            try Task.checkCancellation()
            var outputPointer: UnsafeMutablePointer<UInt8>?
            var outputLength = 0
            let result = session.withUnsafeBytes { bytes in
                unpeel_native_bridge_remote_desktop_clear(
                    handle,
                    bytes.bindMemory(to: UInt8.self).baseAddress,
                    bytes.count,
                    &outputPointer,
                    &outputLength
                )
            }
            return try Self.decodeEffect(
                result: result,
                pointer: outputPointer,
                length: outputLength,
                operation: "desktop resize"
            )
        }
    }

    func markRead(sessionID: String) async throws -> NativeRemoteEffectReceipt {
        let handle = try currentIdentityValidatedHandle()
        let session = Data(sessionID.utf8)
        return try await Self.runBlocking(priority: .utility) {
            try Task.checkCancellation()
            var outputPointer: UnsafeMutablePointer<UInt8>?
            var outputLength = 0
            let result = session.withUnsafeBytes { bytes in
                unpeel_native_bridge_remote_mark_read(
                    handle,
                    bytes.bindMemory(to: UInt8.self).baseAddress,
                    bytes.count,
                    &outputPointer,
                    &outputLength
                )
            }
            return try Self.decodeEffect(
                result: result,
                pointer: outputPointer,
                length: outputLength,
                operation: "mark Session read"
            )
        }
    }

    // MARK: Session organization/lifecycle verbs

    /// The plain `(handle, session_id) → receipt` effect verbs share one C
    /// shape; the imported function pointers are `@convention(c)` and safe to
    /// pass across the blocking hop.
    private typealias SessionEffectFFI = @convention(c) (
        unpeel_native_bridge_remote_handle_t,
        UnsafePointer<UInt8>?,
        Int,
        UnsafeMutablePointer<UnsafeMutablePointer<UInt8>?>?,
        UnsafeMutablePointer<Int>?
    ) -> Int32

    private func performSessionEffect(
        sessionID: String,
        operation: String,
        call: SessionEffectFFI
    ) async throws -> NativeRemoteEffectReceipt {
        let handle = try currentIdentityValidatedHandle()
        let session = Data(sessionID.utf8)
        return try await Self.runBlocking(priority: .userInitiated) {
            // Cancellation before dispatch is safely not-applied. Never check
            // again after the FFI call: a landed effect's receipt must win.
            try Task.checkCancellation()
            var outputPointer: UnsafeMutablePointer<UInt8>?
            var outputLength = 0
            let result = session.withUnsafeBytes { bytes in
                call(
                    handle,
                    bytes.bindMemory(to: UInt8.self).baseAddress,
                    bytes.count,
                    &outputPointer,
                    &outputLength
                )
            }
            return try Self.decodeEffect(
                result: result,
                pointer: outputPointer,
                length: outputLength,
                operation: operation
            )
        }
    }

    func setSessionTitle(
        sessionID: String,
        title: String
    ) async throws -> NativeRemoteEffectReceipt {
        let handle = try currentIdentityValidatedHandle()
        let session = Data(sessionID.utf8)
        let titleData = Data(title.utf8)
        return try await Self.runBlocking(priority: .userInitiated) {
            try Task.checkCancellation()
            var outputPointer: UnsafeMutablePointer<UInt8>?
            var outputLength = 0
            let result = session.withUnsafeBytes { sessionBytes in
                titleData.withUnsafeBytes { titleBytes in
                    unpeel_native_bridge_remote_session_title_set(
                        handle,
                        sessionBytes.bindMemory(to: UInt8.self).baseAddress,
                        sessionBytes.count,
                        titleBytes.bindMemory(to: UInt8.self).baseAddress,
                        titleBytes.count,
                        &outputPointer,
                        &outputLength
                    )
                }
            }
            return try Self.decodeEffect(
                result: result,
                pointer: outputPointer,
                length: outputLength,
                operation: "session title"
            )
        }
    }

    func setSessionPinned(
        sessionID: String,
        pinned: Bool
    ) async throws -> NativeRemoteEffectReceipt {
        let handle = try currentIdentityValidatedHandle()
        let session = Data(sessionID.utf8)
        let pinnedFlag: Int32 = pinned ? 1 : 0
        return try await Self.runBlocking(priority: .userInitiated) {
            try Task.checkCancellation()
            var outputPointer: UnsafeMutablePointer<UInt8>?
            var outputLength = 0
            let result = session.withUnsafeBytes { bytes in
                unpeel_native_bridge_remote_session_pinned_set(
                    handle,
                    bytes.bindMemory(to: UInt8.self).baseAddress,
                    bytes.count,
                    pinnedFlag,
                    &outputPointer,
                    &outputLength
                )
            }
            return try Self.decodeEffect(
                result: result,
                pointer: outputPointer,
                length: outputLength,
                operation: "session pin"
            )
        }
    }

    func setSessionNotifyWhenDone(
        sessionID: String,
        enabled: Bool
    ) async throws -> NativeRemoteEffectReceipt {
        let handle = try currentIdentityValidatedHandle()
        let session = Data(sessionID.utf8)
        let enabledFlag: Int32 = enabled ? 1 : 0
        return try await Self.runBlocking(priority: .userInitiated) {
            try Task.checkCancellation()
            var outputPointer: UnsafeMutablePointer<UInt8>?
            var outputLength = 0
            let result = session.withUnsafeBytes { bytes in
                unpeel_native_bridge_remote_session_notify_when_done_set(
                    handle,
                    bytes.bindMemory(to: UInt8.self).baseAddress,
                    bytes.count,
                    enabledFlag,
                    &outputPointer,
                    &outputLength
                )
            }
            return try Self.decodeEffect(
                result: result,
                pointer: outputPointer,
                length: outputLength,
                operation: "notify when done"
            )
        }
    }

    func answerApproval(
        id: String,
        approved: Bool
    ) async throws -> NativeRemoteEffectReceipt {
        let handle = try currentIdentityValidatedHandle()
        let approval = Data(id.utf8)
        let approvedFlag: Int32 = approved ? 1 : 0
        return try await Self.runBlocking(priority: .userInitiated) {
            try Task.checkCancellation()
            var outputPointer: UnsafeMutablePointer<UInt8>?
            var outputLength = 0
            let result = approval.withUnsafeBytes { bytes in
                unpeel_native_bridge_remote_approval_answer(
                    handle,
                    bytes.bindMemory(to: UInt8.self).baseAddress,
                    bytes.count,
                    approvedFlag,
                    &outputPointer,
                    &outputLength
                )
            }
            return try Self.decodeEffect(
                result: result,
                pointer: outputPointer,
                length: outputLength,
                operation: "approval answer"
            )
        }
    }

    func setSessionProject(
        sessionID: String,
        projectID: String
    ) async throws -> NativeRemoteEffectReceipt {
        let handle = try currentIdentityValidatedHandle()
        let session = Data(sessionID.utf8)
        let project = Data(projectID.utf8)
        return try await Self.runBlocking(priority: .userInitiated) {
            try Task.checkCancellation()
            var outputPointer: UnsafeMutablePointer<UInt8>?
            var outputLength = 0
            let result = session.withUnsafeBytes { sessionBytes in
                project.withUnsafeBytes { projectBytes in
                    unpeel_native_bridge_remote_session_project_set(
                        handle,
                        sessionBytes.bindMemory(to: UInt8.self).baseAddress,
                        sessionBytes.count,
                        projectBytes.bindMemory(to: UInt8.self).baseAddress,
                        projectBytes.count,
                        &outputPointer,
                        &outputLength
                    )
                }
            }
            return try Self.decodeEffect(
                result: result,
                pointer: outputPointer,
                length: outputLength,
                operation: "session project"
            )
        }
    }

    func archiveSession(sessionID: String) async throws -> NativeRemoteEffectReceipt {
        try await performSessionEffect(
            sessionID: sessionID,
            operation: "session archive",
            call: unpeel_native_bridge_remote_session_archive
        )
    }

    func restoreSession(sessionID: String) async throws -> NativeRemoteEffectReceipt {
        try await performSessionEffect(
            sessionID: sessionID,
            operation: "session restore",
            call: unpeel_native_bridge_remote_session_restore
        )
    }

    func stopSession(sessionID: String) async throws -> NativeRemoteEffectReceipt {
        try await performSessionEffect(
            sessionID: sessionID,
            operation: "session stop",
            call: unpeel_native_bridge_remote_session_stop
        )
    }

    func removeSession(sessionID: String) async throws -> NativeRemoteEffectReceipt {
        try await performSessionEffect(
            sessionID: sessionID,
            operation: "session remove",
            call: unpeel_native_bridge_remote_session_remove
        )
    }

    func restartSession(sessionID: String) async throws -> NativeRemoteEffectReceipt {
        try await performSessionEffect(
            sessionID: sessionID,
            operation: "session restart",
            call: unpeel_native_bridge_remote_session_restart
        )
    }

    func resumeAgent(sessionID: String) async throws -> NativeRemoteEffectReceipt {
        try await performSessionEffect(
            sessionID: sessionID,
            operation: "session agent resume",
            call: unpeel_native_bridge_remote_session_resume_agent
        )
    }

    func setSessionOrder(
        projectID: String,
        orderedSessionIDs: [String]
    ) async throws -> NativeRemoteEffectReceipt {
        let handle = try currentIdentityValidatedHandle()
        let project = Data(projectID.utf8)
        let orderedJSON: Data
        do {
            orderedJSON = try JSONEncoder().encode(orderedSessionIDs)
        } catch {
            throw NativeRemoteBackendError(
                result: Int32(UNPEEL_NATIVE_BRIDGE_ERROR_SERIALIZATION),
                code: "invalid_session_order",
                message: "Could not encode the new Session order.",
                kind: "notApplied",
                operation: "session order"
            )
        }
        return try await Self.runBlocking(priority: .userInitiated) {
            try Task.checkCancellation()
            var outputPointer: UnsafeMutablePointer<UInt8>?
            var outputLength = 0
            let result = project.withUnsafeBytes { projectBytes in
                orderedJSON.withUnsafeBytes { orderedBytes in
                    unpeel_native_bridge_remote_session_order_set(
                        handle,
                        projectBytes.bindMemory(to: UInt8.self).baseAddress,
                        projectBytes.count,
                        orderedBytes.bindMemory(to: UInt8.self).baseAddress,
                        orderedBytes.count,
                        &outputPointer,
                        &outputLength
                    )
                }
            }
            return try Self.decodeEffect(
                result: result,
                pointer: outputPointer,
                length: outputLength,
                operation: "session order"
            )
        }
    }

    /// Supported subset of the shared patch on the bridge wire; the Host
    /// route rejects `folderID`, so it is never encoded here.
    private struct ProjectOrganizationPatchWire: Encodable {
        let sortOrder: Int?
        let displayName: String?
        let colorID: String?
        let dateSorted: Bool?
        let pinned: Bool?
    }

    func setProjectOrganization(
        projectID: String,
        patch: RemoteProjectOrganizationPatch
    ) async throws -> NativeRemoteEffectReceipt {
        let handle = try currentIdentityValidatedHandle()
        let project = Data(projectID.utf8)
        let patchJSON: Data
        do {
            patchJSON = try JSONEncoder().encode(ProjectOrganizationPatchWire(
                sortOrder: patch.sortOrder,
                displayName: patch.displayName,
                colorID: patch.colorID,
                dateSorted: patch.dateSorted,
                pinned: patch.pinned
            ))
        } catch {
            throw NativeRemoteBackendError(
                result: Int32(UNPEEL_NATIVE_BRIDGE_ERROR_SERIALIZATION),
                code: "invalid_project_organization",
                message: "Could not encode the project organization patch.",
                kind: "notApplied",
                operation: "project organization"
            )
        }
        return try await Self.runBlocking(priority: .userInitiated) {
            try Task.checkCancellation()
            var outputPointer: UnsafeMutablePointer<UInt8>?
            var outputLength = 0
            let result = project.withUnsafeBytes { projectBytes in
                patchJSON.withUnsafeBytes { patchBytes in
                    unpeel_native_bridge_remote_project_organization_set(
                        handle,
                        projectBytes.bindMemory(to: UInt8.self).baseAddress,
                        projectBytes.count,
                        patchBytes.bindMemory(to: UInt8.self).baseAddress,
                        patchBytes.count,
                        &outputPointer,
                        &outputLength
                    )
                }
            }
            return try Self.decodeEffect(
                result: result,
                pointer: outputPointer,
                length: outputLength,
                operation: "project organization"
            )
        }
    }

    func setPreset(patch: RemotePresetPatch) async throws -> NativeRemoteEffectReceipt {
        let handle = try currentIdentityValidatedHandle()
        let patchJSON: Data
        do {
            patchJSON = try JSONEncoder().encode(patch)
        } catch {
            throw NativeRemoteBackendError(
                result: Int32(UNPEEL_NATIVE_BRIDGE_ERROR_SERIALIZATION),
                code: "invalid_preset_patch",
                message: "Could not encode the preset patch.",
                kind: "notApplied",
                operation: "preset edit"
            )
        }
        return try await Self.runBlocking(priority: .userInitiated) {
            try Task.checkCancellation()
            var outputPointer: UnsafeMutablePointer<UInt8>?
            var outputLength = 0
            let result = patchJSON.withUnsafeBytes { bytes in
                unpeel_native_bridge_remote_preset_set(
                    handle,
                    bytes.bindMemory(to: UInt8.self).baseAddress,
                    bytes.count,
                    &outputPointer,
                    &outputLength
                )
            }
            return try Self.decodeEffect(
                result: result,
                pointer: outputPointer,
                length: outputLength,
                operation: "preset edit"
            )
        }
    }

    func setWorkspaceSettings(
        patch: RemoteWorkspaceSettingsPatch
    ) async throws -> NativeRemoteEffectReceipt {
        let handle = try currentIdentityValidatedHandle()
        let patchJSON: Data
        do {
            patchJSON = try JSONEncoder().encode(patch)
        } catch {
            throw NativeRemoteBackendError(
                result: Int32(UNPEEL_NATIVE_BRIDGE_ERROR_SERIALIZATION),
                code: "invalid_workspace_settings_patch",
                message: "Could not encode the workspace settings patch.",
                kind: "notApplied",
                operation: "workspace settings"
            )
        }
        return try await Self.runBlocking(priority: .userInitiated) {
            try Task.checkCancellation()
            var outputPointer: UnsafeMutablePointer<UInt8>?
            var outputLength = 0
            let result = patchJSON.withUnsafeBytes { bytes in
                unpeel_native_bridge_remote_workspace_settings_set(
                    handle,
                    bytes.bindMemory(to: UInt8.self).baseAddress,
                    bytes.count,
                    &outputPointer,
                    &outputLength
                )
            }
            return try Self.decodeEffect(
                result: result,
                pointer: outputPointer,
                length: outputLength,
                operation: "workspace settings"
            )
        }
    }

    func createSession(
        _ request: RemoteCreateSessionRequest
    ) async throws -> NativeRemoteCreatedSession {
        let handle = try currentIdentityValidatedHandle()
        let requestJSON: Data
        do {
            requestJSON = try JSONEncoder().encode(request)
        } catch {
            throw NativeRemoteBackendError(
                result: Int32(UNPEEL_NATIVE_BRIDGE_ERROR_SERIALIZATION),
                code: "invalid_session_create_request",
                message: "Could not encode the new-Session request.",
                kind: "notApplied",
                operation: "session create"
            )
        }
        return try await Self.runBlocking(priority: .userInitiated) {
            try Task.checkCancellation()
            var outputPointer: UnsafeMutablePointer<UInt8>?
            var outputLength = 0
            let result = requestJSON.withUnsafeBytes { bytes in
                unpeel_native_bridge_remote_session_create(
                    handle,
                    bytes.bindMemory(to: UInt8.self).baseAddress,
                    bytes.count,
                    &outputPointer,
                    &outputLength
                )
            }
            let output = Self.takeOutput(outputPointer, length: outputLength)
            guard result == UNPEEL_NATIVE_BRIDGE_OK else {
                throw Self.effectBridgeError(
                    result: result,
                    output: output,
                    operation: "session create"
                )
            }
            do {
                return try JSONDecoder().decode(NativeRemoteCreatedSession.self, from: output)
            } catch {
                throw NativeRemoteBackendError(
                    result: Int32(UNPEEL_NATIVE_BRIDGE_ERROR_SERIALIZATION),
                    code: "invalid_remote_created_session",
                    message: "The Host may have created the Session, but returned an invalid receipt. Refresh the Host before continuing.",
                    kind: "outcomeUnknown",
                    operation: "session create"
                )
            }
        }
    }

    func pairingInvitation(_ requestJSON: Data) async throws -> Data {
        let handle = try currentIdentityValidatedHandle()
        guard !requestJSON.isEmpty else {
            throw Self.invalidInput(
                code: "invalid_pairing_invitation",
                message: "The pairing invitation request is empty."
            )
        }
        return try await Self.runBlocking(priority: .userInitiated) {
            try Task.checkCancellation()
            var outputPointer: UnsafeMutablePointer<UInt8>?
            var outputLength = 0
            let result = requestJSON.withUnsafeBytes { bytes in
                unpeel_native_bridge_remote_pairing_invitation(
                    handle,
                    bytes.bindMemory(to: UInt8.self).baseAddress,
                    bytes.count,
                    &outputPointer,
                    &outputLength
                )
            }
            let output = Self.takeOutput(outputPointer, length: outputLength)
            guard result == UNPEEL_NATIVE_BRIDGE_OK else {
                throw Self.effectBridgeError(
                    result: result,
                    output: output,
                    operation: "pairing invitation"
                )
            }
            return output
        }
    }

    func uploadAttachment(
        sessionID: String?,
        contentType: String,
        bytes: Data
    ) async throws -> String {
        let handle = try currentIdentityValidatedHandle()
        guard !bytes.isEmpty else {
            throw Self.invalidInput(
                code: "invalid_upload",
                message: "The upload is empty."
            )
        }
        let session = Data((sessionID ?? "").utf8)
        let type = Data(contentType.utf8)
        struct UploadReceipt: Decodable { let path: String }
        return try await Self.runBlocking(priority: .userInitiated) {
            try Task.checkCancellation()
            var outputPointer: UnsafeMutablePointer<UInt8>?
            var outputLength = 0
            let result = session.withUnsafeBytes { sessionBytes in
                type.withUnsafeBytes { typeBytes in
                    bytes.withUnsafeBytes { payloadBytes in
                        unpeel_native_bridge_remote_upload_attachment(
                            handle,
                            sessionBytes.bindMemory(to: UInt8.self).baseAddress,
                            sessionBytes.count,
                            typeBytes.bindMemory(to: UInt8.self).baseAddress,
                            typeBytes.count,
                            payloadBytes.bindMemory(to: UInt8.self).baseAddress,
                            payloadBytes.count,
                            &outputPointer,
                            &outputLength
                        )
                    }
                }
            }
            let output = Self.takeOutput(outputPointer, length: outputLength)
            guard result == UNPEEL_NATIVE_BRIDGE_OK else {
                throw Self.effectBridgeError(
                    result: result,
                    output: output,
                    operation: "attachment upload"
                )
            }
            guard let receipt = try? JSONDecoder().decode(UploadReceipt.self, from: output),
                  receipt.path.hasPrefix("/")
            else {
                throw Self.invalidInput(
                    code: "invalid_upload_receipt",
                    message: "The Host did not return the uploaded file's path."
                )
            }
            return receipt.path
        }
    }

    func listArchivedSessions(projectID: String) async throws -> [RemoteSessionSummary] {
        let handle = try currentIdentityValidatedHandle()
        let project = Data(projectID.utf8)
        return try await Self.runBlocking(priority: .userInitiated) {
            try Task.checkCancellation()
            var outputPointer: UnsafeMutablePointer<UInt8>?
            var outputLength = 0
            let result = project.withUnsafeBytes { bytes in
                unpeel_native_bridge_remote_archived_sessions(
                    handle,
                    bytes.bindMemory(to: UInt8.self).baseAddress,
                    bytes.count,
                    &outputPointer,
                    &outputLength
                )
            }
            let output = Self.takeOutput(outputPointer, length: outputLength)
            guard result == UNPEEL_NATIVE_BRIDGE_OK else {
                throw Self.bridgeError(
                    result: result,
                    output: output,
                    fallbackCode: "remote_archive_read_failed",
                    fallbackMessage: "Could not load this project's archived Sessions."
                )
            }
            do {
                let response = try JSONDecoder().decode(
                    RemoteArchivedSessionsResponse.self,
                    from: output
                )
                guard response.projectID == projectID else {
                    throw NativeRemoteBackendError(
                        result: Int32(UNPEEL_NATIVE_BRIDGE_ERROR_SERIALIZATION),
                        code: "remote_archive_project_mismatch",
                        message: "The Host returned archived Sessions for the wrong project."
                    )
                }
                return response.sessions
            } catch let error as NativeRemoteBackendError {
                throw error
            } catch {
                throw NativeRemoteBackendError(
                    result: Int32(UNPEEL_NATIVE_BRIDGE_ERROR_SERIALIZATION),
                    code: "invalid_remote_archive_response",
                    message: "The Host returned an invalid archived-Sessions response."
                )
            }
        }
    }

    func transcriptMarkdown(
        sessionID: String,
        entries: Int?
    ) async throws -> RemoteTranscriptMarkdown {
        let handle = try currentIdentityValidatedHandle()
        let session = Data(sessionID.utf8)
        let boundedEntries = UInt32(clamping: max(0, entries ?? 0))
        return try await Self.runBlocking(priority: .userInitiated) {
            try Task.checkCancellation()
            var outputPointer: UnsafeMutablePointer<UInt8>?
            var outputLength = 0
            let result = session.withUnsafeBytes { bytes in
                unpeel_native_bridge_remote_transcript_markdown(
                    handle,
                    bytes.bindMemory(to: UInt8.self).baseAddress,
                    bytes.count,
                    boundedEntries,
                    &outputPointer,
                    &outputLength
                )
            }
            let output = Self.takeOutput(outputPointer, length: outputLength)
            guard result == UNPEEL_NATIVE_BRIDGE_OK else {
                throw Self.bridgeError(
                    result: result,
                    output: output,
                    fallbackCode: "remote_transcript_read_failed",
                    fallbackMessage: "Could not load this Session's transcript."
                )
            }
            do {
                let transcript = try JSONDecoder().decode(
                    RemoteTranscriptMarkdown.self,
                    from: output
                )
                guard transcript.sessionID == sessionID else {
                    throw NativeRemoteBackendError(
                        result: Int32(UNPEEL_NATIVE_BRIDGE_ERROR_SERIALIZATION),
                        code: "remote_transcript_session_mismatch",
                        message: "The Host returned a transcript for the wrong Session."
                    )
                }
                return transcript
            } catch let error as NativeRemoteBackendError {
                throw error
            } catch {
                throw NativeRemoteBackendError(
                    result: Int32(UNPEEL_NATIVE_BRIDGE_ERROR_SERIALIZATION),
                    code: "invalid_remote_transcript",
                    message: "The Host returned an invalid transcript response."
                )
            }
        }
    }

    func sessionMetrics(sessionID: String) async throws -> NativeRemoteSessionMetrics {
        let handle = try currentIdentityValidatedHandle()
        let session = Data(sessionID.utf8)
        return try await Self.runBlocking(priority: .userInitiated) {
            try Task.checkCancellation()
            var outputPointer: UnsafeMutablePointer<UInt8>?
            var outputLength = 0
            let result = session.withUnsafeBytes { bytes in
                unpeel_native_bridge_remote_session_metrics(
                    handle,
                    bytes.bindMemory(to: UInt8.self).baseAddress,
                    bytes.count,
                    &outputPointer,
                    &outputLength
                )
            }
            let output = Self.takeOutput(outputPointer, length: outputLength)
            guard result == UNPEEL_NATIVE_BRIDGE_OK else {
                throw Self.bridgeError(
                    result: result,
                    output: output,
                    fallbackCode: "remote_metrics_read_failed",
                    fallbackMessage: "Could not read this Session's terminal size."
                )
            }
            do {
                let metrics = try JSONDecoder().decode(
                    NativeRemoteSessionMetrics.self,
                    from: output
                )
                guard metrics.sessionID == sessionID else {
                    throw NativeRemoteBackendError(
                        result: Int32(UNPEEL_NATIVE_BRIDGE_ERROR_SERIALIZATION),
                        code: "remote_metrics_session_mismatch",
                        message: "The Host returned terminal metrics for the wrong Session."
                    )
                }
                return metrics
            } catch let error as NativeRemoteBackendError {
                throw error
            } catch {
                throw NativeRemoteBackendError(
                    result: Int32(UNPEEL_NATIVE_BRIDGE_ERROR_SERIALIZATION),
                    code: "invalid_remote_metrics",
                    message: "The Host returned an invalid terminal-metrics response."
                )
            }
        }
    }

    /// Idempotent from Swift's perspective. The first caller owns the Rust
    /// close; later callers see an already-closed object and do nothing.
    func close() async {
        guard let handle = takeHandle() else { return }
        await Task.detached(priority: .utility) {
            Self.closeIgnoringError(handle)
        }.value
    }

    var isClosed: Bool {
        lock.lock()
        defer { lock.unlock() }
        return handle == 0
    }

    private func currentHandle() throws -> unpeel_native_bridge_remote_handle_t {
        lock.lock()
        defer { lock.unlock() }
        guard handle != 0 else {
            throw NativeRemoteBackendError(
                result: Int32(UNPEEL_NATIVE_BRIDGE_ERROR_INVALID_HANDLE),
                code: "remote_backend_closed",
                message: "This remote Host connection is closed."
            )
        }
        return handle
    }

    private func currentIdentityValidatedHandle() throws
        -> unpeel_native_bridge_remote_handle_t
    {
        lock.lock()
        defer { lock.unlock() }
        guard handle != 0 else {
            throw NativeRemoteBackendError(
                result: Int32(UNPEEL_NATIVE_BRIDGE_ERROR_INVALID_HANDLE),
                code: "remote_backend_closed",
                message: "This remote Host connection is closed."
            )
        }
        guard identityValidatedHandle == handle else {
            throw NativeRemoteBackendError(
                result: Int32(UNPEEL_NATIVE_BRIDGE_ERROR_REMOTE),
                code: "remote_backend_not_bootstrapped",
                message: "Load and verify this Host before reading or controlling Sessions."
            )
        }
        return handle
    }

    private func markIdentityValidated(
        for expectedHandle: unpeel_native_bridge_remote_handle_t
    ) throws {
        lock.lock()
        defer { lock.unlock() }
        guard handle == expectedHandle, handle != 0 else {
            throw NativeRemoteBackendError(
                result: Int32(UNPEEL_NATIVE_BRIDGE_ERROR_INVALID_HANDLE),
                code: "remote_backend_closed",
                message: "The remote Host connection closed while its identity was verified."
            )
        }
        identityValidatedHandle = expectedHandle
    }

    private func takeHandle() -> unpeel_native_bridge_remote_handle_t? {
        lock.lock()
        defer { lock.unlock() }
        guard handle != 0 else { return nil }
        let value = handle
        handle = 0
        identityValidatedHandle = nil
        return value
    }

    private static func closeIgnoringError(_ handle: unpeel_native_bridge_remote_handle_t) {
        var outputPointer: UnsafeMutablePointer<UInt8>?
        var outputLength = 0
        _ = unpeel_native_bridge_remote_close(
            handle,
            &outputPointer,
            &outputLength
        )
        _ = takeOutput(outputPointer, length: outputLength)
    }

    fileprivate static func discardIgnoringError(
        parentHandle: unpeel_native_bridge_remote_handle_t,
        pageHandle: unpeel_native_bridge_remote_output_page_handle_t
    ) {
        var outputPointer: UnsafeMutablePointer<UInt8>?
        var outputLength = 0
        _ = unpeel_native_bridge_remote_output_discard(
            parentHandle,
            pageHandle,
            &outputPointer,
            &outputLength
        )
        _ = takeOutput(outputPointer, length: outputLength)
    }

    static func decodeEffect(
        result: Int32,
        pointer: UnsafeMutablePointer<UInt8>?,
        length: Int,
        operation: String
    ) throws -> NativeRemoteEffectReceipt {
        let output = takeOutput(pointer, length: length)
        guard result == UNPEEL_NATIVE_BRIDGE_OK else {
            throw effectBridgeError(
                result: result,
                output: output,
                operation: operation
            )
        }
        do {
            return try JSONDecoder().decode(NativeRemoteEffectReceipt.self, from: output)
        } catch {
            throw NativeRemoteBackendError(
                result: Int32(UNPEEL_NATIVE_BRIDGE_ERROR_SERIALIZATION),
                code: "invalid_remote_effect_receipt",
                message: "The remote Host may have applied \(operation), but returned an invalid receipt. Reconnect before continuing.",
                kind: "outcomeUnknown",
                operation: operation
            )
        }
    }

    static func effectBridgeError(
        result: Int32,
        output: Data,
        operation: String
    ) -> NativeRemoteBackendError {
        let object = try? JSONSerialization.jsonObject(with: output) as? [String: Any]
        let kind = object?["kind"] as? String
        if kind == "notApplied" || kind == "outcomeUnknown" {
            return NativeRemoteBackendError(
                result: result,
                code: object?["code"] as? String ?? "remote_effect_failed",
                message: object?["message"] as? String
                    ?? "Could not apply remote \(operation).",
                kind: kind,
                operation: object?["operation"] as? String ?? operation
            )
        }
        // The effect ABI promises a delivery classification on every error.
        // If that envelope is corrupt, retrying could duplicate an effect.
        return NativeRemoteBackendError(
            result: result,
            code: "invalid_remote_effect_failure",
            message: "Delivery of remote \(operation) is uncertain because the Host returned an invalid failure receipt. Reconnect before continuing.",
            kind: "outcomeUnknown",
            operation: operation
        )
    }

    private static func invalidInput(
        code: String,
        message: String
    ) -> NativeRemoteBackendError {
        NativeRemoteBackendError(
            result: Int32(UNPEEL_NATIVE_BRIDGE_ERROR_INVALID_INPUT),
            code: code,
            message: message
        )
    }

    private static func runBlocking<T: Sendable>(
        priority: TaskPriority,
        _ operation: @escaping @Sendable () throws -> T
    ) async throws -> T {
        let worker = Task.detached(priority: priority, operation: operation)
        return try await withTaskCancellationHandler {
            try await worker.value
        } onCancel: {
            worker.cancel()
        }
    }

    private static func takeOutput(
        _ pointer: UnsafeMutablePointer<UInt8>?,
        length: Int
    ) -> Data {
        guard let pointer, length > 0 else { return Data() }
        let data = Data(bytes: pointer, count: length)
        unpeel_native_bridge_free(pointer, length)
        return data
    }

    /// Shared only with the Link callback adapter. Returned bridge buffers
    /// always remain Rust-owned and are freed through the C ABI exactly once.
    static func takeBridgeOutput(
        _ pointer: UnsafeMutablePointer<UInt8>?,
        length: Int
    ) -> Data {
        takeOutput(pointer, length: length)
    }

    private static func bridgeError(
        result: Int32,
        output: Data,
        fallbackCode: String,
        fallbackMessage: String
    ) -> NativeRemoteBackendError {
        let object = try? JSONSerialization.jsonObject(with: output) as? [String: Any]
        return NativeRemoteBackendError(
            result: result,
            code: object?["code"] as? String ?? fallbackCode,
            message: object?["error"] as? String
                ?? object?["message"] as? String
                ?? "\(fallbackMessage) (\(result))",
            kind: object?["kind"] as? String,
            operation: object?["operation"] as? String
        )
    }

    static func bridgeFailure(
        result: Int32,
        output: Data,
        fallbackCode: String,
        fallbackMessage: String
    ) -> NativeRemoteBackendError {
        bridgeError(
            result: result,
            output: output,
            fallbackCode: fallbackCode,
            fallbackMessage: fallbackMessage
        )
    }

    static func validateHostIdentity(
        _ snapshot: RemoteBootstrapSnapshot,
        expectedHostID: String?
    ) throws {
        guard let expectedHostID else { return }
        guard snapshot.macID == expectedHostID else {
            throw NativeRemoteBackendError(
                result: Int32(UNPEEL_NATIVE_BRIDGE_ERROR_REMOTE),
                code: "host_identity_changed",
                message: "Refusing a remote Host whose identity no longer matches the saved Host."
            )
        }
    }
}

extension NativeRemoteBackend: NativeRemoteBackendProtocol {}
