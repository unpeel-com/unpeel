//
//  RemoteHostRuntime.swift
//  UnpeelNative
//
//  Remote-only observable state. It deliberately does not project Host rows
//  into UnpeelStore's Local projects/sessions or SurfaceCache.
//

import Foundation
import UnpeelShared

enum RemoteHostConnectionState: Equatable, Sendable {
    case idle
    case connecting
    case connected(name: String)
    /// The last valid sidebar/terminal remains visible while reconnection runs.
    case reconnecting(message: String)
    case repairRequired(message: String)
    case incompatible(message: String)
    case failed(message: String)
}

/// User-facing route for one Host connection. The UI deliberately exposes
/// only the useful distinction (local network or Unpeel Link), never relay
/// endpoints, tokens, or a manual transport picker.
enum RemoteHostConnectionRoute: Equatable, Sendable {
    case ssh
    case localGateway
    case direct
    case link

    var shortLabel: String {
        switch self {
        case .ssh: "Connected"
        case .localGateway: "This Mac"
        case .direct: "Direct"
        case .link: "Via Link"
        }
    }
}

/// One Host-originated focus decision for a local direct terminal data plane.
/// Ordinary clicks and launcher presentation remain Controller-window state;
/// only a correlated create/restart result crosses this one-way seam.
struct DirectDataPlaneSelectionIntent: Equatable, Sendable {
    let sequence: UInt64
    let sessionID: String?
}

/// A transport is only a carrier for the shared Host contract. Keeping this
/// choice at the runtime boundary prevents the native UI from acquiring
/// transport-specific session behavior.
enum RemoteHostTransport: Sendable {
    case ssh(
        target: String,
        expectedHostID: String?,
        mode: RemoteSSHConnectionMode,
        secret: String?
    )
    /// Another LOCAL workspace, scoped through the loopback gateway
    /// (workspaces-unification phase 2). `expectedHostID` is the workspace's
    /// persisted `mobile/mac-id`, nil for a never-started workspace whose
    /// first bootstrap pins it — exactly like an unknown-identity SSH Host.
    case localGateway(
        unpeelHome: String,
        workspaceName: String,
        expectedHostID: String?
    )
    /// This app instance's own workspace over the persistent `unpeel serve`
    /// worker. Semantic reads and organization/lifecycle effects use the
    /// local framed Host contract, while terminal bytes stay on the direct
    /// `unpeel-attach` data plane. The transport refuses the legacy in-child
    /// semantic Host fallback when `host.sock` is unavailable.
    case localService(
        unpeelHome: String,
        workspaceName: String,
        expectedHostID: String?
    )
    case direct(endpoint: URL, authToken: String, expectedHostID: String)
    case link(
        credentials: RelayCredentials,
        controllerDeviceID: String,
        authToken: String,
        expectedHostID: String
    )

    fileprivate var expectedHostID: String? {
        switch self {
        case let .ssh(_, expectedHostID, _, _): expectedHostID
        case let .localGateway(_, _, expectedHostID): expectedHostID
        case let .localService(_, _, expectedHostID): expectedHostID
        case let .direct(_, _, expectedHostID): expectedHostID
        case let .link(_, _, _, expectedHostID): expectedHostID
        }
    }

    fileprivate var route: RemoteHostConnectionRoute {
        switch self {
        case .ssh: .ssh
        case .localGateway, .localService: .localGateway
        case .direct: .direct
        case .link: .link
        }
    }

    /// Local terminal rendering remains the existing `unpeel-attach` →
    /// `session.sock` path. This runtime may still submit explicit semantic
    /// Host verbs, but must not poll output, fit, mark read, or accept terminal
    /// input merely because the worker advertises those operations.
    fileprivate var usesDirectSessionDataPlane: Bool {
        switch self {
        case .localService, .localGateway:
            // Both homes are on this disk: the surface runs `unpeel-attach`
            // on the workspace's own `session.sock`, exactly like Local
            // scope. Only sidebar/lifecycle verbs ride this connection.
            return true
        case .ssh, .direct, .link:
            return false
        }
    }

    fileprivate var continuityKey: RemoteHostContinuityKey {
        if let expectedHostID {
            return .pinnedHost(expectedHostID)
        }
        switch self {
        case let .ssh(target, _, _, _):
            return .sshTarget(target)
        case let .localGateway(unpeelHome, _, _):
            return .workspaceHome(unpeelHome)
        case let .localService(unpeelHome, _, _):
            return .workspaceHome(unpeelHome)
        case .direct, .link:
            // Paired transports always require a saved Host identity.
            preconditionFailure("A paired Host transport must be identity-pinned")
        }
    }
}

private struct PairedHostConnectionPlan: Sendable {
    let hostID: String
    let direct: RemoteHostTransport
    /// Nil when the user scoped this Host to Direct-only
    /// (`PairedHostRecord.linkEnabled == false`): no Link fallback, grace
    /// race, or background probe may ever open the relay downlink for it.
    let link: RemoteHostTransport?
}

private enum RemoteHostContinuityKey: Hashable, Sendable {
    case pinnedHost(String)
    case sshTarget(String)
    case workspaceHome(String)

    var paneFallbackKey: String {
        switch self {
        case let .pinnedHost(hostID): "host:\(hostID)"
        case let .sshTarget(target): "ssh:\(target)"
        case let .workspaceHome(home): "workspace:\(home)"
        }
    }
}



@MainActor
final class RemoteHostRuntime: ObservableObject {
    typealias BackendFactory = (
        _ transport: RemoteHostTransport
    ) throws -> any NativeRemoteBackendProtocol

    typealias PaneAttachmentProbe = @MainActor @Sendable (
        _ pane: RemoteGhosttyTerminalPane
    ) -> Bool
    typealias PaneOutputFeeder = @MainActor @Sendable (
        _ pane: RemoteGhosttyTerminalPane,
        _ bytes: Data,
        _ resetBeforeFeed: Bool
    ) -> Bool

    @Published private(set) var snapshot: RemoteBootstrapSnapshot?
    @Published private(set) var selectedSessionID: String?
    @Published private(set) var directDataPlaneSelectionIntent:
        DirectDataPlaneSelectionIntent?
    @Published private(set) var connectionState: RemoteHostConnectionState = .idle
    @Published private(set) var connectionRoute: RemoteHostConnectionRoute?
    /// False only after a successful bootstrap for the current connection.
    /// In particular, an outcome-unknown effect closes this gate until a
    /// later bootstrap proves a fresh, accepted connection generation.
    @Published private(set) var terminalEffectsEnabled = false

    /// True only while an explicit create/restart is waiting for the exact
    /// Session row it should focus on a direct terminal data plane. The Local
    /// store uses this to avoid replacing that intent with its pre-effect row.
    var directDataPlaneSelectionIntentPending: Bool {
        connection?.transport.usesDirectSessionDataPlane == true
            && (pendingCreatedSelectionID != nil || pendingReplacementSelection != nil)
    }

    var selectionConnectionIsActive: Bool {
        guard connection != nil, refreshTask != nil else { return false }
        return switch connectionState {
        case .connecting, .connected, .reconnecting:
            true
        case .idle, .repairRequired, .incompatible, .failed:
            false
        }
    }

    /// Whether the live, still-active connection already serves this exact
    /// continuity target. Scope re-entry uses this to ADOPT the warm
    /// background connection instead of reopening a backend — reuse can
    /// never duplicate an in-flight semantic effect the way a reopen could,
    /// and the retained snapshot/pane cache make the return instant. Any
    /// identity mismatch fails closed into the normal connect path.
    func warmConnectionMatches(
        pinnedHostID: String?,
        workspaceHome: String? = nil
    ) -> Bool {
        guard selectionConnectionIsActive, let continuityKey else { return false }
        if let pinnedHostID, continuityKey == .pinnedHost(pinnedHostID) {
            return true
        }
        if let workspaceHome, continuityKey == .workspaceHome(workspaceHome) {
            return true
        }
        return false
    }

    /// Present a pool-cached bootstrap for the just-connected target so scope
    /// entry renders real content instantly (workspaces-unification phase 7).
    /// Presentation-only by construction: `connectionBootstrapped` and
    /// `terminalEffectsEnabled` stay false, no pane host key is derived, and
    /// no output pump or effect worker can start — the first real bootstrap
    /// replaces the seed (or fails identity closed) exactly as without one.
    /// Rejected whenever a snapshot is already published (same-Host
    /// reconnects retain their own) or the seed's identity does not match
    /// the pinned/expected Host.
    func seedSnapshot(_ seed: RemoteBootstrapSnapshot) {
        guard let connection, !connectionBootstrapped, snapshot == nil else { return }
        if let expected = connection.transport.expectedHostID,
           seed.macID != expected {
            return
        }
        if let pinnedHostID, let seedHostID = seed.macID, seedHostID != pinnedHostID {
            return
        }
        snapshot = seed
        if selectedSessionID == nil, !replacementDefaultSelectionSuppressed {
            selectedSessionID = Self.defaultSessionID(in: seed)
            if !presentedSessionsAreViewOwned {
                presentedTerminalSessionIDs = selectedSessionID.map { Set([$0]) } ?? []
            }
        }
    }

    private final class ActiveConnection: @unchecked Sendable {
        let epoch: UInt64
        let transport: RemoteHostTransport
        let backend: any NativeRemoteBackendProtocol
        let effectStartBarrier: Task<Void, Never>?
        let desktopFits: DesktopFitOwnership
        let markReadsSuppressedUntilPostBarrier: Set<String>
        var needsPostBarrierRefresh: Bool

        init(
            epoch: UInt64,
            transport: RemoteHostTransport,
            backend: any NativeRemoteBackendProtocol,
            effectStartBarrier: Task<Void, Never>? = nil,
            desktopFits: DesktopFitOwnership = DesktopFitOwnership(),
            markReadsSuppressedUntilPostBarrier: Set<String> = [],
            needsPostBarrierRefresh: Bool = false
        ) {
            self.epoch = epoch
            self.transport = transport
            self.backend = backend
            self.effectStartBarrier = effectStartBarrier
            self.desktopFits = desktopFits
            self.markReadsSuppressedUntilPostBarrier =
                markReadsSuppressedUntilPostBarrier
            self.needsPostBarrierRefresh = needsPostBarrierRefresh
        }
    }

    /// Fit state belongs to the exact backend generation that received it.
    /// The lock lets retirement cleanup wait for an in-flight effect worker,
    /// observe which clears actually landed, then compensate only the fits
    /// that are still owned before closing that backend.
    private final class DesktopFitOwnership: @unchecked Sendable {
        private let lock = NSLock()
        private var sessionIDs: Set<String> = []
        private var inheritedClearPending = false

        func insert(_ sessionID: String) {
            lock.lock()
            sessionIDs.insert(sessionID)
            lock.unlock()
        }

        func remove(_ sessionID: String) {
            lock.lock()
            sessionIDs.remove(sessionID)
            lock.unlock()
        }

        func snapshot() -> Set<String> {
            lock.lock()
            defer { lock.unlock() }
            return sessionIDs
        }

        func armInheritedClear() {
            lock.lock()
            inheritedClearPending = true
            lock.unlock()
        }

        func hasInheritedClearPending() -> Bool {
            lock.lock()
            defer { lock.unlock() }
            return inheritedClearPending
        }

        func claimInheritedFitsForClear() -> Set<String> {
            lock.lock()
            defer { lock.unlock() }
            guard inheritedClearPending else { return [] }
            inheritedClearPending = false
            return sessionIDs
        }
    }

    /// Owns a speculative route backend until it is either promoted or
    /// closed. Cancellation can race a blocking FFI bootstrap, so close must
    /// be independently callable and exactly once.
    private final class RouteProbeCandidate: @unchecked Sendable {
        let backend: any NativeRemoteBackendProtocol
        private let lock = NSLock()
        private var closed = false

        init(backend: any NativeRemoteBackendProtocol) {
            self.backend = backend
        }

        func close() async {
            guard claimClose() else { return }
            await backend.close()
        }

        private func claimClose() -> Bool {
            lock.lock()
            defer { lock.unlock() }
            let shouldClose = !closed
            closed = true
            return shouldClose
        }
    }

    private struct OutputPumpIdentity: Equatable, Sendable {
        let token: UInt64
        let connectionEpoch: UInt64
        let paneKey: RemoteTerminalPaneKey
        let paneIdentity: ObjectIdentifier
    }

    private struct PaneCursorIdentity: Equatable, Sendable {
        let connectionEpoch: UInt64
        let paneIdentity: ObjectIdentifier
    }

    private struct RequestedDesktopFit: Equatable, Sendable {
        let columns: UInt16
        let rows: UInt16
    }

    private enum RemoteEffect: Sendable {
        case write(sessionID: String, data: Data)
        case fit(sessionID: String, columns: UInt16, rows: UInt16)
        case clearFit(sessionID: String)
        case markRead(sessionID: String)

        var sessionID: String {
            switch self {
            case let .write(sessionID, _),
                 let .fit(sessionID, _, _),
                 let .clearFit(sessionID),
                 let .markRead(sessionID):
                sessionID
            }
        }

        func perform(
            on backend: any NativeRemoteBackendProtocol
        ) async throws -> NativeRemoteEffectReceipt {
            switch self {
            case let .write(sessionID, data):
                try await backend.writeTerminal(sessionID: sessionID, data: data)
            case let .fit(sessionID, columns, rows):
                try await backend.fitDesktop(
                    sessionID: sessionID,
                    columns: columns,
                    rows: rows
                )
            case let .clearFit(sessionID):
                try await backend.clearDesktopFit(sessionID: sessionID)
            case let .markRead(sessionID):
                try await backend.markRead(sessionID: sessionID)
            }
        }
    }

    private struct QueuedEffect: Sendable {
        var effect: RemoteEffect
        var notAppliedRetryCount: UInt8 = 0
    }

    private struct OutstandingRetirement {
        let task: Task<Void, Never>
        let pendingMarkReads: Set<String>
        let awaitingMarkReads: Set<String>
        let failedMarkReads: Set<String>
    }

    private let backendFactory: BackendFactory
    private let refreshIntervalNanoseconds: UInt64
    private let initialBootstrapRetryIntervalNanoseconds: UInt64
    private let initialBootstrapFastRetryCount: Int
    private let initialDirectLinkGraceNanoseconds: UInt64
    private let directProbeSuccessfulLinkRefreshes: Int
    private let forceLinkForDevelopment: Bool
    private let outputIdleIntervalNanoseconds: UInt64
    private let resizeDebounceNanoseconds: UInt64
    /// Fits ship only after a Session stays presented this long (0 in
    /// tests): the fly-by-switch guard.
    private let fitSettleInterval: TimeInterval
    /// How long an un-presented Session keeps its desktop fit before the
    /// clear ships (0 in tests = immediate): the round-trip churn guard.
    private let fitClearDelayNanoseconds: UInt64
    private let paneAttachmentProbe: PaneAttachmentProbe
    private let paneOutputFeeder: PaneOutputFeeder
    private let paneCache: RemoteGhosttyPaneCache

    private var connection: ActiveConnection?
    private var refreshTask: Task<Void, Never>?
    /// Set while the refresh loop is parked in its inter-poll sleep; resuming
    /// it wakes the loop so a just-written local-against-home state edit shows
    /// up in the scoped sidebar without waiting the full poll interval.
    private struct RefreshWakeWaiter {
        let token: UInt64
        let continuation: CheckedContinuation<Void, Never>
    }
    private var refreshWakeWaiter: RefreshWakeWaiter?
    private var refreshWakeToken: UInt64 = 0
    private var outputTasks: [RemoteTerminalPaneKey: Task<Void, Never>] = [:]
    private var outputPumpIdentities: [RemoteTerminalPaneKey: OutputPumpIdentity] = [:]
    private var effectWorker: Task<Void, Never>?
    private var resizeTasks: [RemoteTerminalPaneKey: Task<Void, Never>] = [:]
    private var routeProbeTask: Task<Void, Never>?
    private var directLinkGraceTask: Task<Void, Never>?
    private var routeProbeTarget: RemoteHostConnectionRoute?
    private var routeProbeCandidate: RouteProbeCandidate?

    private var connectionEpoch: UInt64 = 0
    /// Changes whenever remote I/O is invalidated, even if the backend object
    /// remains current. A bootstrap may publish only if it began after the
    /// latest failure, so an older health poll cannot reopen a failed gate.
    private var recoveryEpoch: UInt64 = 0
    private var outputPumpToken: UInt64 = 0
    private var effectQueueEpoch: UInt64 = 0
    private var effectWorkerToken: UInt64 = 0
    private var routeProbeToken: UInt64 = 0
    private var pairedPlanEpoch: UInt64 = 0
    private var continuityKey: RemoteHostContinuityKey?
    private var pinnedHostID: String?
    private var paneHostKey: String?
    private var connectionBootstrapped = false
    private var pairedPlan: PairedHostConnectionPlan?
    private var successfulLinkRefreshes = 0
    /// A transport may fail while an effect worker is unwinding. Retain every
    /// such tail so promotion can prove the old backend is quiescent before
    /// closing it or starting effects on its replacement.
    private var retiredEffectTails: [Task<Void, Never>] = []
    /// Disconnecting or a failed replacement open must not discard the old
    /// same-Host effect/fit barrier. A later Local → Host return consumes it
    /// by durable Host continuity before it may bootstrap or send effects.
    private var outstandingRetirements: [
        RemoteHostContinuityKey: OutstandingRetirement
    ] = [:]

    private var queuedEffects: [QueuedEffect] = []
    private var pendingWriteBytes = 0

    private struct DeferredTerminalInput {
        let sessionID: String
        var data: Data
    }

    /// Terminal input typed while remote I/O is transiently closed — the
    /// initial bootstrap, or a reconnect after a transport failure with no
    /// write in flight. Bounded and ordered; flushed onto the effect queue
    /// once the next bootstrap is accepted, discarded when ordering becomes
    /// uncertain or the selection changes. This never replays a dispatched
    /// effect: only bytes the Host provably never received are held.
    private var deferredTerminalInput: [DeferredTerminalInput] = []
    private var deferredTerminalInputBytes = 0
    /// A dispatched write's outcome is unknown, or one was in flight when
    /// the transport failed. Input typed afterwards must not be delivered
    /// on the next generation: it could land after — or without — the
    /// uncertain bytes. Cleared by the next accepted bootstrap.
    private var terminalInputOrderingUncertain = false
    /// Effect-queue epoch of the write currently awaiting its receipt.
    private var writeInFlightQueueEpoch: UInt64?
    static let maximumDeferredTerminalInputBytes = 64 * 1024
    private var pendingMarkReadSessionIDs: Set<String> = []
    /// A failed route may still have a mark-read effect unwinding. No route
    /// may reconsider those ids until this tail settles and a bootstrap that
    /// started afterward proves the Host still reports them unread.
    private var markReadRecoveryBarrier: Task<Void, Never>?
    private var markReadsSuppressedUntilRecovery: Set<String> = []
    private var markReadAwaitingSnapshotClearSessionIDs: Set<String> = []
    private var failedAutomaticMarkReadSessionIDs: Set<String> = []
    private var pendingDesktopFitClearSessionIDs: Set<String> = []
    private var enqueuedDesktopFitClearSessionIDs: Set<String> = []
    private var latestViewportByPane: [RemoteTerminalPaneKey: RequestedDesktopFit] = [:]
    private var lastQueuedViewportBySession: [String: RequestedDesktopFit] = [:]
    private var failedFitBySession: [String: RequestedDesktopFit] = [:]
    private var incompleteUTF8InputBySession: [String: Data] = [:]
    private var paneCursorReady: [RemoteTerminalPaneKey: PaneCursorIdentity] = [:]
    /// Sessions with an attached terminal surface in the content area. The
    /// sidebar selection remains the pane-group representative; this set lets
    /// every visible VT pane stream, resize, and accept input without changing
    /// that navigation identity.
    private var presentedTerminalSessionIDs: Set<String> = []
    /// Last explicitly selected Session per scope (continuity key), for the
    /// whole app run: returning to a workspace/Host re-selects where you
    /// left off instead of falling to the Host's default row.
    private var lastSelectionByScope: [String: String] = [:]
    /// When each Session joined the presented set — desktop fits are held
    /// until a Session has stayed presented briefly, so rapid workspace or
    /// session flips never thrash the Host PTY with resizes (each SIGWINCH
    /// makes the TUI redraw mid-stream; interleaved widths garble the
    /// frame, and equal final sizes never trigger a cleanup repaint).
    private var presentedAtBySession: [String: Date] = [:]
    private var settleFitTasks: [String: Task<Void, Never>] = [:]
    /// Deferred fit clears: leaving a Session doesn't clear its desktop fit
    /// immediately — a quick return would otherwise pay TWO Host PTY
    /// resizes (clear, then re-fit) and two TUI redraws per round trip.
    /// The clear ships only after the Session stays un-presented for a few
    /// seconds; returning sooner cancels it, and the identical re-fit then
    /// dedupes to no resize at all.
    private var deferredFitClearTasks: [String: Task<Void, Never>] = [:]
    /// Reconciliation loop for presented sessions' fits (see
    /// reassertPresentedDesktopFits).
    private var fitReassertTask: Task<Void, Never>?
    /// Before a content view mounts, selection is the legacy fallback for the
    /// presented set (and keeps headless unit seams useful). Once the view has
    /// explicitly reported its mounted panes, navigation selection must never
    /// collapse that renderer-owned set.
    private var presentedSessionsAreViewOwned = false
    /// A Controller-created Session may not be in the published snapshot yet.
    /// The first bootstrap that reports it selects it, exactly like a local
    /// spawn selects its new row.
    private var pendingCreatedSelectionID: String?
    /// Correlated create receipts can carry a complete starting row before the
    /// detached Session Host has published its first manifest. Retain that row
    /// across a few racing bootstraps so creation feels local and never flashes
    /// back to the previous selection.
    private var pendingOptimisticCreatedSession:
        (summary: RemoteSessionSummary, observationsRemaining: Int)?
    private var directDataPlaneSelectionSequence: UInt64 = 0
    /// Restore & Resume mints a new Session id but the legacy success receipt
    /// carries only `{ok:true}`. Correlate the replacement against a snapshot
    /// latched before either effect, and fail closed on collisions instead of
    /// adopting whichever row happens to sort first.
    private var pendingReplacementSelection: ReplacementSelectionIntent?
    /// Once a combined restore/restart invalidates the selected id, only an
    /// exact correlation or explicit user choice may select another row.
    /// This remains set after ambiguity/expiry so a later health poll cannot
    /// turn a failed correlation into an arbitrary fallback.
    private var replacementDefaultSelectionSuppressed = false

    struct ReplacementSelectionIntent: Equatable {
        static let maximumBootstrapObservations = 30

        let sourceSessionID: String
        let projectID: String
        let createdAtUnixMs: Int64
        /// Provider/runtime identity derived from providerID or the command
        /// family. The replacement command itself contains resume flags, so
        /// exact command-string equality would reject the real replacement.
        let runtimeID: String?
        let worktreePath: String?
        let worktreeBranch: String?
        let baselineSessionIDs: Set<String>
        var bootstrapObservationsRemaining: Int

        init(
            source: RemoteSessionSummary,
            knownSessionIDs: Set<String>,
            bootstrapObservationsRemaining: Int = maximumBootstrapObservations
        ) {
            sourceSessionID = source.id
            projectID = source.projectID
            createdAtUnixMs = source.createdAtUnixMs
            runtimeID = source.providerID ?? SetupTool.detect(in: source.command)?.id
            worktreePath = source.worktreePath
            worktreeBranch = source.worktreeBranch
            baselineSessionIDs = knownSessionIDs.union([source.id])
            self.bootstrapObservationsRemaining = bootstrapObservationsRemaining
        }
    }

    enum ReplacementSelectionResolution: Equatable {
        case wait(ReplacementSelectionIntent)
        case select(String)
        case cancel
    }

    /// The Host rejects a larger write. Input callbacks may be much larger
    /// (notably paste), so the runtime batches them without changing byte
    /// order. The pending bound prevents a fast producer from retaining an
    /// unbounded paste stream while a Host is slow or offline.
    static let maximumTerminalWriteBytes = 64 * 1024
    static let maximumPendingTerminalInputBytes = 4 * 1024 * 1024

    init(
        refreshIntervalNanoseconds: UInt64 = 2_000_000_000,
        initialBootstrapRetryIntervalNanoseconds: UInt64 = 100_000_000,
        initialBootstrapFastRetryCount: Int = 3,
        initialDirectLinkGraceNanoseconds: UInt64 = 750_000_000,
        directProbeSuccessfulLinkRefreshes: Int = 8,
        forceLinkForDevelopment: Bool = RemoteHostRuntime.defaultForceLinkForDevelopment,
        outputIdleIntervalNanoseconds: UInt64 = 50_000_000,
        resizeDebounceNanoseconds: UInt64 = 80_000_000,
        fitSettleNanoseconds: UInt64 = 300_000_000,
        fitClearDelayNanoseconds: UInt64 = 20_000_000_000,
        backendFactory: @escaping BackendFactory = { transport in
            switch transport {
            case let .ssh(target, expectedHostID, mode, secret):
                try NativeRemoteBackend(
                    sshTarget: target,
                    expectedHostID: expectedHostID,
                    mode: mode,
                    secret: secret
                )
            case let .localGateway(unpeelHome, _, expectedHostID):
                try NativeRemoteBackend(
                    localGatewayHome: unpeelHome,
                    expectedHostID: expectedHostID
                )
            case let .localService(unpeelHome, _, expectedHostID):
                try NativeRemoteBackend(
                    localGatewayHome: unpeelHome,
                    expectedHostID: expectedHostID,
                    requireHostService: true
                )
            case let .direct(endpoint, authToken, expectedHostID):
                try NativeRemoteBackend(
                    directEndpoint: endpoint,
                    authToken: authToken,
                    expectedHostID: expectedHostID
                )
            case let .link(credentials, controllerDeviceID, authToken, expectedHostID):
                try NativeRemoteBackend(
                    relayCredentials: credentials,
                    controllerDeviceID: controllerDeviceID,
                    authToken: authToken,
                    expectedHostID: expectedHostID
                )
            }
        },
        paneCache: RemoteGhosttyPaneCache = RemoteGhosttyPaneCache(),
        paneAttachmentProbe: @escaping PaneAttachmentProbe = { $0.isReadyForHostBytes },
        paneOutputFeeder: @escaping PaneOutputFeeder = { pane, bytes, reset in
            pane.receiveHostBytes(bytes, resetBeforeFeed: reset)
        }
    ) {
        self.refreshIntervalNanoseconds = refreshIntervalNanoseconds
        self.initialBootstrapRetryIntervalNanoseconds =
            initialBootstrapRetryIntervalNanoseconds
        self.initialBootstrapFastRetryCount = max(0, initialBootstrapFastRetryCount)
        self.initialDirectLinkGraceNanoseconds = initialDirectLinkGraceNanoseconds
        self.directProbeSuccessfulLinkRefreshes = max(
            1,
            directProbeSuccessfulLinkRefreshes
        )
        self.forceLinkForDevelopment = forceLinkForDevelopment
        self.outputIdleIntervalNanoseconds = outputIdleIntervalNanoseconds
        self.resizeDebounceNanoseconds = resizeDebounceNanoseconds
        fitSettleInterval = Double(fitSettleNanoseconds) / 1_000_000_000
        self.fitClearDelayNanoseconds = fitClearDelayNanoseconds
        self.backendFactory = backendFactory
        self.paneCache = paneCache
        self.paneAttachmentProbe = paneAttachmentProbe
        self.paneOutputFeeder = paneOutputFeeder
    }

    deinit {
        refreshTask?.cancel()
        refreshWakeWaiter?.continuation.resume()
        refreshWakeWaiter = nil
        outputTasks.values.forEach { $0.cancel() }
        resizeTasks.values.forEach { $0.cancel() }
        directLinkGraceTask?.cancel()
        routeProbeTask?.cancel()
        if let routeProbeCandidate {
            Task { await routeProbeCandidate.close() }
        }
        if let connection {
            let priorWorkers = retiredEffectTails + [effectWorker].compactMap { $0 }
            Task {
                for worker in priorWorkers {
                    await worker.value
                }
                await Self.clearOwnedDesktopFitsAndClose(connection)
            }
        }
    }

    static var defaultForceLinkForDevelopment: Bool {
        guard RemoteHostFeature.pickerEnabled else { return false }
        return UserDefaults.standard.bool(forKey: "unpeel.native.forceLink")
    }

    /// SSH is the advanced/admin fallback. It carries the same Host contract
    /// as direct pairing; no SSH-specific behavior exists above this call.
    func connectSSH(
        target: String,
        expectedHostID: String?,
        mode: RemoteSSHConnectionMode = .command,
        secret: String? = nil
    ) {
        connect(.ssh(
            target: target,
            expectedHostID: expectedHostID,
            mode: mode,
            secret: secret
        ), pairedPlan: nil)
    }

    /// Loopback gateway to another LOCAL workspace on this Mac. Same Host
    /// contract as every remote transport; the gateway child is spawned (and
    /// killed on disconnect) by the Rust connection.
    func connectLocalWorkspace(
        home: String,
        name: String,
        expectedHostID: String?
    ) {
        connect(.localGateway(
            unpeelHome: home,
            workspaceName: name,
            expectedHostID: expectedHostID
        ), pairedPlan: nil)
    }

    /// Connect this app's Local scope to its canonical persistent workspace
    /// worker. The transport is read/effect-identical to every other Host;
    /// only the no-fallback launch policy differs from a compatibility
    /// `localGateway` connection.
    func connectLocalService(
        home: String,
        name: String,
        expectedHostID: String?
    ) {
        connect(.localService(
            unpeelHome: home,
            workspaceName: name,
            expectedHostID: expectedHostID
        ), pairedPlan: nil)
    }

    /// Normal paired-LAN App-to-TUI/App-to-App connection.
    func connectDirect(endpoint: URL, authToken: String, expectedHostID: String) {
        connect(.direct(
            endpoint: endpoint,
            authToken: authToken,
            expectedHostID: expectedHostID
        ), pairedPlan: nil)
    }

    /// Normal paired App-to-App/App-to-TUI connection. Every selection starts
    /// on the saved LAN endpoint and falls back to the paired Link channel only
    /// for reachability failures. The paired record's Host id is the durable
    /// identity pin for both routes.
    func connectPairedHost(
        record: PairedHostRecord,
        credentials: RemoteHostCredentials
    ) {
        // A Direct-only Host (removed from the Unpeel Link enrollment list)
        // gets no Link transport at all: reachability failures then report
        // Direct-only reachability instead of silently riding the relay.
        let link: RemoteHostTransport? = record.isLinkEnabled
            ? .link(
                credentials: credentials.relayCredentials,
                controllerDeviceID: record.controllerDeviceID,
                authToken: credentials.authToken,
                expectedHostID: record.hostID
            )
            : nil
        let plan = PairedHostConnectionPlan(
            hostID: record.hostID,
            direct: .direct(
                endpoint: record.endpoint,
                authToken: credentials.authToken,
                expectedHostID: record.hostID
            ),
            link: link
        )
        connect(
            forceLinkForDevelopment ? (plan.link ?? plan.direct) : plan.direct,
            pairedPlan: plan
        )
    }

    func disconnect() {
        pairedPlanEpoch &+= 1
        cancelInitialDirectLinkGrace()
        cancelRouteProbe()
        connectionEpoch &+= 1
        let retiringKey = continuityKey
        let existingRetirement = retiringKey.flatMap {
            outstandingRetirements.removeValue(forKey: $0)
        }
        let retiringPendingMarkReads = pendingMarkReadSessionIDs
            .union(existingRetirement?.pendingMarkReads ?? [])
        let retiringAwaitingMarkReads = markReadAwaitingSnapshotClearSessionIDs
            .union(existingRetirement?.awaitingMarkReads ?? [])
        let retiringFailedMarkReads = failedAutomaticMarkReadSessionIDs
            .union(existingRetirement?.failedMarkReads ?? [])
        let oldConnection = connection
        let priorWorker = invalidateConnectionWork()
        connection = nil
        let retirementPrerequisite = combineTasks(
            existingRetirement?.task,
            priorWorker
        )
        let finalRetirement = close(
            oldConnection,
            after: retirementPrerequisite
        ) ?? retirementPrerequisite
        if let retiringKey {
            storeOutstandingRetirement(
                for: retiringKey,
                task: finalRetirement,
                pendingMarkReads: retiringPendingMarkReads,
                awaitingMarkReads: retiringAwaitingMarkReads,
                failedMarkReads: retiringFailedMarkReads
            )
        }
        continuityKey = nil
        pinnedHostID = nil
        paneHostKey = nil
        snapshot = nil
        selectedSessionID = nil
        pendingCreatedSelectionID = nil
        pendingOptimisticCreatedSession = nil
        directDataPlaneSelectionIntent = nil
        pendingReplacementSelection = nil
        replacementDefaultSelectionSuppressed = false
        connectionState = .idle
        connectionRoute = nil
        pairedPlan = nil
        successfulLinkRefreshes = 0
        paneCache.removeAll()
        presentedTerminalSessionIDs.removeAll()
        latestViewportByPane.removeAll()
        paneCursorReady.removeAll()
        incompleteUTF8InputBySession.removeAll()
    }

    /// Keep the selected remote scope fail-closed when its saved pairing was
    /// minted for a different Controller identity. The only recovery is an
    /// explicit re-pair; presenting idle/disconnected would hide that fact.
    func requirePairingRepair() {
        disconnect()
        connectionState = .repairRequired(
            message: "This Host was paired with a different Controller identity. Pair it again."
        )
    }

    func selectSession(_ sessionID: String) {
        guard snapshot?.sessions.contains(where: { $0.id == sessionID }) == true else {
            return
        }
        // Explicit user selection always wins over an earlier optimistic
        // create/replacement intent; no later bootstrap may steal focus back.
        pendingCreatedSelectionID = nil
        pendingOptimisticCreatedSession = nil
        pendingReplacementSelection = nil
        replacementDefaultSelectionSuppressed = false
        guard selectedSessionID != sessionID else {
            ensurePresentedOutputPumps()
            requestMarkReadIfNeeded(sessionID)
            return
        }

        transitionSelectedSession(to: sessionID)
        requestMarkReadIfNeeded(sessionID)
        prunePaneCache()
    }

    /// Synchronize Controller-window selection for a transport whose terminal
    /// bytes do not ride this runtime. This intentionally cancels stale create
    /// or replacement focus intents without sending mark-read, starting an
    /// output pump, or claiming desktop-fit ownership.
    func selectDirectDataPlaneSession(_ sessionID: String?) {
        guard connection?.transport.usesDirectSessionDataPlane == true else { return }
        guard sessionID == nil
            || snapshot?.sessions.contains(where: { $0.id == sessionID }) == true
        else { return }
        pendingCreatedSelectionID = nil
        pendingOptimisticCreatedSession = nil
        pendingReplacementSelection = nil
        replacementDefaultSelectionSuppressed = false
        transitionSelectedSession(to: sessionID)
        replacePresentedTerminalSessions([])
        prunePaneCache()
    }

    /// Replace the exact set of terminal panes mounted by the shared content
    /// area. A group keeps its sidebar representative and every member
    /// presented concurrently; removing a pane cancels only that Session's
    /// output/resize work and releases its desktop-fit ownership.
    func setPresentedTerminalSessions(_ sessionIDs: Set<String>) {
        presentedSessionsAreViewOwned = true
        replacePresentedTerminalSessions(sessionIDs)
    }

    private func replacePresentedTerminalSessions(_ sessionIDs: Set<String>) {
        let validIDs = Set(snapshot?.sessions.map(\.id) ?? [])
        let next = sessionIDs.intersection(validIDs)
        guard next != presentedTerminalSessionIDs else {
            ensurePresentedOutputPumps()
            enqueueLatestPresentedDesktopFits()
            return
        }

        let removed = presentedTerminalSessionIDs.subtracting(next)
        let added = next.subtracting(presentedTerminalSessionIDs)
        presentedTerminalSessionIDs = next
        for sessionID in added {
            // A blip (the view re-sets the presented set constantly) must
            // not reset the settle clock — only a real departure (the
            // deferred clear firing) forgets the presentation age.
            deferredFitClearTasks.removeValue(forKey: sessionID)?.cancel()
            if presentedAtBySession[sessionID] == nil {
                presentedAtBySession[sessionID] = Date()
            }
        }
        for sessionID in removed {
            settleFitTasks.removeValue(forKey: sessionID)?.cancel()
            guard let key = paneKey(for: sessionID) else { continue }
            stopOutputPump(for: key)
            resizeTasks.removeValue(forKey: key)?.cancel()
            deferredFitClearTasks[sessionID]?.cancel()
            guard fitClearDelayNanoseconds > 0 else {
                presentedAtBySession.removeValue(forKey: sessionID)
                requestDesktopFitClear(sessionID)
                continue
            }
            let clearDelay = fitClearDelayNanoseconds
            deferredFitClearTasks[sessionID] = Task { [weak self] in
                // Generous on purpose: the clear exists for letterbox
                // arbitration (a phone taking over sends its own fit
                // regardless), while every clear+refit round trip costs two
                // PTY resizes and two TUI redraws — the transient garble
                // window. Ordinary session/workspace hopping stays entirely
                // resize-free inside this horizon.
                try? await Task.sleep(nanoseconds: clearDelay)
                guard let self, !Task.isCancelled else { return }
                self.deferredFitClearTasks.removeValue(forKey: sessionID)
                guard !self.terminalSessionIsPresented(sessionID) else { return }
                self.presentedAtBySession.removeValue(forKey: sessionID)
                self.requestDesktopFitClear(sessionID)
            }
        }
        rebindPresentedPanesAndEnsurePumps()
        enqueueLatestPresentedDesktopFits()
        updateFitReassertLoop()
        for sessionID in next {
            requestMarkReadIfNeeded(sessionID)
        }
        prunePaneCache()
    }

    /// Focus a visible in-memory pane without changing the pane group's
    /// representative sidebar selection. Used by its collapsed runtime marks.
    @discardableResult
    func focusTerminalPane(_ sessionID: String) -> Bool {
        guard terminalSessionIsPresented(sessionID),
              let pane = terminalPane(for: sessionID)
        else { return false }
        requestMarkReadIfNeeded(sessionID)
        pane.focus()
        pane.renderNow()
        return true
    }

    /// Return the remote-only, in-memory Ghostty pane for a Host Session. The
    /// cache key includes Host identity, so identical Session ids on two Hosts
    /// can never share VT state. The runtime owns the pane and its callbacks;
    /// the UI owns only attaching/presenting the returned NSView.
    func terminalPane(
        for sessionID: String,
        style: TerminalPaneStyle = .resolved(),
        workingDirectory: String? = nil,
        onCommandClick: ((ClickablePath.Match, String) -> Bool)? = nil
    ) -> RemoteGhosttyTerminalPane? {
        guard snapshot?.sessions.contains(where: { $0.id == sessionID }) == true,
              let paneHostKey
        else {
            return nil
        }

        let key = RemoteTerminalPaneKey(
            hostID: paneHostKey,
            sessionID: sessionID
        )
        // A failed reconnect may have no active backend while the last valid
        // Host snapshot and VT frame are still useful. Return that exact pane
        // read-only; its old callbacks are epoch-gated and cannot reach a
        // retired connection. Never create a new pane without a connection.
        guard let connection else {
            return paneCache.existingPane(for: key)
        }
        let existingPane = paneCache.existingPane(for: key)
        let pane = paneCache.pane(
            for: key,
            style: style,
            onInput: makeInputHandler(
                sessionID: sessionID,
                paneKey: key,
                connectionEpoch: connection.epoch
            ),
            onResize: makeResizeHandler(
                sessionID: sessionID,
                paneKey: key,
                connectionEpoch: connection.epoch
            ),
            workingDirectory: workingDirectory,
            onCommandClick: onCommandClick
        )
        if existingPane == nil || existingPane !== pane {
            paneCursorReady.removeValue(forKey: key)
        }
        paneCache.noteShown(key)
        // Creation can take the cache over its LRU limit. Prune before a
        // pump captures pane identity, so an immediately evicted pane can
        // never reset or advance the backend cursor.
        prunePaneCache()
        guard paneCache.existingPane(for: key) === pane else { return nil }
        if terminalSessionIsPresented(sessionID) {
            ensureOutputPump(for: sessionID)
            requestMarkReadIfNeeded(sessionID)
        }
        return pane
    }

    /// Explicit effect seams are useful to non-pane controls and keep every
    /// mutation on the same serial, generation-bound chain as key input.
    func sendTerminalInput(_ data: Data, to sessionID: String) {
        guard !data.isEmpty, terminalSessionIsPresented(sessionID) else { return }
        guard let prepared = prepareTerminalInput(data, sessionID: sessionID),
              !prepared.isEmpty
        else { return }
        if enqueueEffect(.write(sessionID: sessionID, data: prepared)) { return }
        deferTerminalInputIfTransient(sessionID: sessionID, data: prepared)
    }

    /// Hold refused input only while the gate is closed for a transient
    /// reason (a bootstrap is in flight for a selected remote Host). A
    /// bootstrapped connection that still refuses — missing input
    /// capability, unknown Session — is a permanent answer, and a direct
    /// data plane never carries input here at all.
    private func deferTerminalInputIfTransient(sessionID: String, data: Data) {
        guard let connection,
              !connection.transport.usesDirectSessionDataPlane,
              !connectionBootstrapped,
              !terminalInputOrderingUncertain
        else { return }
        deferTerminalInput(sessionID: sessionID, data: data)
    }

    private func deferTerminalInput(sessionID: String, data: Data) {
        guard !data.isEmpty,
              deferredTerminalInputBytes + data.count
                  <= Self.maximumDeferredTerminalInputBytes
        else { return }
        deferredTerminalInputBytes += data.count
        if let last = deferredTerminalInput.indices.last,
           deferredTerminalInput[last].sessionID == sessionID {
            deferredTerminalInput[last].data.append(data)
        } else {
            deferredTerminalInput.append(
                DeferredTerminalInput(sessionID: sessionID, data: data)
            )
        }
    }

    private func discardDeferredTerminalInput() {
        deferredTerminalInput.removeAll(keepingCapacity: false)
        deferredTerminalInputBytes = 0
    }

    /// Replay held input onto the freshly accepted generation, in order.
    /// Anything the new snapshot no longer accepts is dropped here.
    private func flushDeferredTerminalInput() {
        let pending = deferredTerminalInput
        discardDeferredTerminalInput()
        for item in pending {
            _ = enqueueEffect(.write(sessionID: item.sessionID, data: item.data))
        }
    }

    func fitDesktop(sessionID: String, columns: Int, rows: Int) {
        guard columns > 0,
              rows > 0,
              let paneKey = paneKey(for: sessionID)
        else {
            return
        }
        let viewport = RequestedDesktopFit(
            columns: UInt16(clamping: columns),
            rows: UInt16(clamping: rows)
        )
        // Recorded even before the Session is presented, mirroring the pane
        // resize path: presentation replays the latest viewport.
        latestViewportByPane[paneKey] = viewport
        guard terminalSessionIsPresented(sessionID) else { return }
        enqueueDesktopFitIfNeeded(
            sessionID: sessionID,
            columns: viewport.columns,
            rows: viewport.rows,
            viewport: viewport
        )
    }

    func clearDesktopFit(sessionID: String) {
        guard snapshot?.sessions.contains(where: { $0.id == sessionID }) == true else {
            return
        }
        requestDesktopFitClear(sessionID)
    }

    func markRead(sessionID: String) {
        guard snapshot?.sessions.contains(where: { $0.id == sessionID }) == true else {
            return
        }
        enqueueEffect(.markRead(sessionID: sessionID))
    }

    private func connect(
        _ transport: RemoteHostTransport,
        pairedPlan nextPairedPlan: PairedHostConnectionPlan?
    ) {
        pairedPlanEpoch &+= 1
        cancelInitialDirectLinkGrace()
        cancelRouteProbe()
        pairedPlan = nextPairedPlan
        successfulLinkRefreshes = 0
        connectionEpoch &+= 1
        let epoch = connectionEpoch
        let oldConnection = connection
        let retiringContinuityKey = continuityKey
        let currentPendingMarkReads = pendingMarkReadSessionIDs
        let currentAwaitingMarkReads = markReadAwaitingSnapshotClearSessionIDs
        let currentFailedMarkReads = failedAutomaticMarkReadSessionIDs
        let nextContinuityKey = transport.continuityKey
        let existingRetirement = outstandingRetirements.removeValue(
            forKey: nextContinuityKey
        )
        let replacesCurrentSameHost = oldConnection != nil
            && continuityKey == nextContinuityKey
        let carriedPendingMarkReads = (existingRetirement?.pendingMarkReads ?? [])
            .union(replacesCurrentSameHost ? currentPendingMarkReads : [])
        let carriedAwaitingMarkReads = (existingRetirement?.awaitingMarkReads ?? [])
            .union(
                replacesCurrentSameHost
                    ? currentAwaitingMarkReads
                    : []
            )
        let carriedFailedMarkReads = (existingRetirement?.failedMarkReads ?? [])
            .union(
                replacesCurrentSameHost ? currentFailedMarkReads : []
            )
        let priorWorker = invalidateConnectionWork()
        pendingMarkReadSessionIDs.formUnion(carriedPendingMarkReads)
        markReadAwaitingSnapshotClearSessionIDs.formUnion(carriedAwaitingMarkReads)
        failedAutomaticMarkReadSessionIDs.formUnion(carriedFailedMarkReads)
        connection = nil
        let retirementPrerequisite = combineTasks(
            existingRetirement?.task,
            priorWorker
        )
        let predecessorRetirement = close(
            oldConnection,
            after: retirementPrerequisite
        ) ?? retirementPrerequisite
        if let retiringContinuityKey,
           retiringContinuityKey != nextContinuityKey,
           oldConnection != nil {
            // Keep the retiring Host's own continuity entry too. If opening
            // the replacement Host fails, a rapid return must still wait the
            // original Host's tail and preserve its semantic latches.
            storeOutstandingRetirement(
                for: retiringContinuityKey,
                task: predecessorRetirement,
                pendingMarkReads: currentPendingMarkReads,
                awaitingMarkReads: currentAwaitingMarkReads,
                failedMarkReads: currentFailedMarkReads
            )
        }

        if continuityKey != nextContinuityKey {
            snapshot = nil
            selectedSessionID = nil
            presentedTerminalSessionIDs.removeAll()
            pendingCreatedSelectionID = nil
            pendingOptimisticCreatedSession = nil
            directDataPlaneSelectionIntent = nil
            pendingReplacementSelection = nil
            replacementDefaultSelectionSuppressed = false
            pinnedHostID = transport.expectedHostID
            paneHostKey = nil
            paneCache.removeAll()
            latestViewportByPane.removeAll()
        } else if let expectedHostID = transport.expectedHostID {
            // A stronger saved identity always wins over an earlier
            // transport-only SSH continuity key.
            pinnedHostID = expectedHostID
        }
        continuityKey = nextContinuityKey
        // A new backend owns a new output-cursor table even when the stable
        // Host and retained VT panes are continuous.
        paneCursorReady.removeAll()
        connectionRoute = transport.route
        connectionState = .connecting

        do {
            let backend = try backendFactory(transport)
            let opened = ActiveConnection(
                epoch: epoch,
                transport: transport,
                backend: backend,
                effectStartBarrier: predecessorRetirement,
                markReadsSuppressedUntilPostBarrier: carriedPendingMarkReads,
                // Even a different-Host replacement must not capture a
                // bootstrap before its transitive predecessor closes: a
                // rapid A → B → A chain otherwise reuses a stale A snapshot.
                needsPostBarrierRefresh: predecessorRetirement != nil
            )
            connection = opened
            startRefreshLoop(for: opened)
            scheduleInitialDirectLinkGraceIfPossible(matching: opened)
        } catch {
            storeOutstandingRetirement(
                for: nextContinuityKey,
                task: predecessorRetirement,
                pendingMarkReads: carriedPendingMarkReads,
                awaitingMarkReads: carriedAwaitingMarkReads,
                failedMarkReads: carriedFailedMarkReads
            )
            connectionState = snapshot == nil
                ? .failed(message: error.localizedDescription)
                : .reconnecting(message: error.localizedDescription)
        }

    }

    private func startRefreshLoop(
        for connection: ActiveConnection,
        initiallyAccepted: Bool = false
    ) {
        cancelRefreshLoop()
        refreshTask = Task { [weak self] in
            guard let self else { return }
            await self.runRefreshLoop(connection, initiallyAccepted: initiallyAccepted)
        }
    }

    private func runRefreshLoop(
        _ candidate: ActiveConnection,
        initiallyAccepted: Bool
    ) async {
        var acceptedBootstrap = initiallyAccepted
        var fastRetriesUsed = 0
        if candidate.needsPostBarrierRefresh {
            await candidate.effectStartBarrier?.value
            guard isCurrent(candidate), !Task.isCancelled else { return }
        } else if initiallyAccepted {
            do {
                try await Task.sleep(nanoseconds: refreshIntervalNanoseconds)
            } catch {
                return
            }
        }
        while !Task.isCancelled {
            if let markReadRecoveryBarrier {
                await markReadRecoveryBarrier.value
                guard isCurrent(candidate), !Task.isCancelled else { return }
            }
            let attemptRecoveryEpoch = recoveryEpoch
            do {
                let next = try await candidate.backend.bootstrap()
                guard isCurrent(candidate) else { return }
                // A terminal/output/effect failure may have happened while
                // this health poll was in flight. Only a bootstrap that began
                // afterward is allowed to reopen remote I/O.
                guard recoveryEpoch == attemptRecoveryEpoch else { continue }
                try validateIdentity(next, for: candidate)
                guard isCurrent(candidate) else { return }

                if markReadRecoveryBarrier != nil {
                    pendingMarkReadSessionIDs.subtract(
                        markReadsSuppressedUntilRecovery
                    )
                    markReadsSuppressedUntilRecovery.removeAll()
                    markReadRecoveryBarrier = nil
                }
                if candidate.needsPostBarrierRefresh {
                    // The predecessor effect tail is now settled and this
                    // snapshot was captured afterward. Only now may an old
                    // pending mark-read be reconsidered; promoting from the
                    // candidate snapshot would silently replay it.
                    pendingMarkReadSessionIDs.subtract(
                        candidate.markReadsSuppressedUntilPostBarrier
                    )
                    candidate.needsPostBarrierRefresh = false
                }
                publishAcceptedBootstrap(next, for: candidate)
                acceptedBootstrap = true
            } catch is CancellationError {
                return
            } catch {
                guard isCurrent(candidate) else { return }
                if retireForTerminalError(error, matching: candidate) {
                    return
                }
                if !acceptedBootstrap,
                   fastRetriesUsed < initialBootstrapFastRetryCount {
                    fastRetriesUsed += 1
                    // Link probing is non-destructive: it cannot promote
                    // until an authenticated bootstrap succeeds, and a Direct
                    // recovery cancels it. Start that proof concurrently so a
                    // black-holed LAN endpoint does not make an off-LAN user
                    // wait through every Direct grace attempt first.
                    scheduleFallbackIfPossible(for: error, matching: candidate)
                    // `pair --serve` may briefly close and rebind the exact
                    // paired endpoint while handing ownership to the TUI.
                    // Bootstrap is a safe read, so absorb only this bounded
                    // pre-first-success window without flashing a failure.
                    connectionBootstrapped = false
                    terminalEffectsEnabled = false
                    stopAllOutputPumps()
                    do {
                        try await Task.sleep(
                            nanoseconds: initialBootstrapRetryIntervalNanoseconds
                        )
                    } catch {
                        return
                    }
                    continue
                }
                disableRemoteIO(error, matching: candidate)
            }

            await interruptibleRefreshSleep()
            if Task.isCancelled { return }
            // A wake means a local-against-home edit just landed; poll now.
        }
    }

    /// Wait one poll interval, but resume early if `requestImmediateRefresh`
    /// fires. Only one waiter parks at a time (the single refresh loop), so a
    /// single stored continuation suffices.
    private func interruptibleRefreshSleep() async {
        refreshWakeToken &+= 1
        let token = refreshWakeToken
        let sleepTask = Task { [refreshIntervalNanoseconds] in
            try? await Task.sleep(nanoseconds: refreshIntervalNanoseconds)
        }
        await withCheckedContinuation { continuation in
            guard !Task.isCancelled else {
                continuation.resume()
                return
            }
            // A replacement loop should have woken its predecessor before
            // reaching this point. Resume defensively rather than ever
            // overwriting (and leaking) an older checked continuation.
            if let prior = refreshWakeWaiter {
                refreshWakeWaiter = nil
                prior.continuation.resume()
            }
            refreshWakeWaiter = RefreshWakeWaiter(
                token: token,
                continuation: continuation
            )
            Task { [weak self] in
                await sleepTask.value
                self?.resumeRefreshWake(token: token)
            }
        }
        sleepTask.cancel()
    }

    private func resumeRefreshWake(token: UInt64? = nil) {
        guard let waiter = refreshWakeWaiter,
              token == nil || token == waiter.token
        else { return }
        refreshWakeWaiter = nil
        waiter.continuation.resume()
    }

    /// Cancellation does not automatically resume a checked continuation.
    /// Always wake a parked refresh before dropping its Task handle, or rapid
    /// workspace replacement leaves the old loop suspended (and retaining
    /// this runtime) forever.
    private func cancelRefreshLoop() {
        refreshTask?.cancel()
        refreshTask = nil
        resumeRefreshWake()
    }

    /// Nudge the refresh loop to bootstrap immediately instead of waiting out
    /// the poll interval — called right after a local-against-home app-state
    /// edit so the new/removed project appears in the scoped sidebar at once.
    /// A no-op when no loop is parked (e.g. mid-bootstrap): the running poll
    /// already re-reads app-state.json on its next cycle.
    func requestImmediateRefresh() {
        resumeRefreshWake()
    }

    private func publishAcceptedBootstrap(
        _ next: RemoteBootstrapSnapshot,
        for candidate: ActiveConnection
    ) {
        cancelInitialDirectLinkGrace()
        adopt(next)
        connectionBootstrapped = true
        if connectionRoute != candidate.transport.route {
            connectionRoute = candidate.transport.route
        }
        let supportsOutput = Self.supportsOutput(in: next)
        let ownsRemoteSessionDataPlane = !candidate.transport.usesDirectSessionDataPlane
        let effectsEnabled = ownsRemoteSessionDataPlane
            && supportsOutput
            && Self.supportsInput(in: next)
        if terminalEffectsEnabled != effectsEnabled {
            terminalEffectsEnabled = effectsEnabled
        }
        let nextState: RemoteHostConnectionState = supportsOutput
            ? .connected(name: next.macName ?? "Host")
            : .incompatible(
                message: "This Host does not advertise remote terminal output."
            )
        if connectionState != nextState { connectionState = nextState }

        switch candidate.transport.route {
        case .direct:
            successfulLinkRefreshes = 0
            // The preferred route recovered before a Link candidate won.
            cancelRouteProbe(target: .link)
        case .link:
            successfulLinkRefreshes += 1
            if successfulLinkRefreshes >= directProbeSuccessfulLinkRefreshes {
                successfulLinkRefreshes = 0
                scheduleDirectProbeIfPossible(matching: candidate)
            }
        case .ssh, .localGateway:
            break
        }

        rebindPresentedPanesAndEnsurePumps()
        if supportsOutput, ownsRemoteSessionDataPlane {
            enqueuePendingDesktopFitClears()
            enqueueLatestPresentedDesktopFits()
            for sessionID in presentedTerminalSessionIDs {
                requestMarkReadIfNeeded(sessionID)
            }
        }
        // A fresh generation: nothing dispatched on it yet, so held input
        // can follow the fits in UI order.
        terminalInputOrderingUncertain = false
        writeInFlightQueueEpoch = nil
        if terminalEffectsEnabled {
            flushDeferredTerminalInput()
        } else {
            discardDeferredTerminalInput()
        }
        startEffectWorkerIfNeeded(candidate)
    }

    private func scheduleFallbackIfPossible(
        for error: Error,
        matching candidate: ActiveConnection
    ) {
        guard Self.failureIsRouteReachability(error),
              let plan = pairedPlan,
              plan.hostID == candidate.transport.expectedHostID
        else { return }

        switch candidate.transport.route {
        case .direct:
            guard let link = plan.link else { return }
            scheduleRouteProbe(link, matching: candidate)
        case .link:
            guard !forceLinkForDevelopment else { return }
            scheduleRouteProbe(plan.direct, matching: candidate)
        case .ssh, .localGateway:
            break
        }
    }

    /// A saved LAN address can black-hole for the full request deadline when
    /// the Controller is away from home. Give Direct a short head start, then
    /// verify Link concurrently; Direct still wins by canceling the candidate
    /// as soon as its bootstrap succeeds.
    private func scheduleInitialDirectLinkGraceIfPossible(
        matching candidate: ActiveConnection
    ) {
        guard candidate.transport.route == .direct,
              let plan = pairedPlan,
              plan.link != nil,
              plan.hostID == candidate.transport.expectedHostID
        else { return }
        directLinkGraceTask = Task { [weak self] in
            guard let self else { return }
            // A safe same-Host reconnect may still be retiring the previous
            // effect/fit generation. Start neither Direct's Link race nor its
            // grace clock before that transitive barrier has settled.
            await candidate.effectStartBarrier?.value
            guard isCurrent(candidate), !Task.isCancelled else { return }
            do {
                try await Task.sleep(nanoseconds: initialDirectLinkGraceNanoseconds)
            } catch {
                return
            }
            guard isCurrent(candidate),
                  !connectionBootstrapped,
                  !Task.isCancelled
            else { return }
            directLinkGraceTask = nil
            guard let link = plan.link else { return }
            scheduleRouteProbe(link, matching: candidate)
        }
    }

    private func cancelInitialDirectLinkGrace() {
        directLinkGraceTask?.cancel()
        directLinkGraceTask = nil
    }

    private func scheduleDirectProbeIfPossible(matching candidate: ActiveConnection) {
        guard !forceLinkForDevelopment,
              candidate.transport.route == .link,
              let plan = pairedPlan,
              plan.hostID == candidate.transport.expectedHostID
        else { return }
        scheduleRouteProbe(plan.direct, matching: candidate)
    }

    private func scheduleRouteProbe(
        _ transport: RemoteHostTransport,
        matching current: ActiveConnection
    ) {
        guard routeProbeTask == nil,
              isCurrent(current),
              transport.route != current.transport.route
        else { return }

        routeProbeToken &+= 1
        let token = routeProbeToken
        let planEpoch = pairedPlanEpoch
        let currentConnectionEpoch = current.epoch
        routeProbeTarget = transport.route
        routeProbeTask = Task { [weak self] in
            guard let self else { return }
            await self.runRouteProbe(
                transport,
                token: token,
                planEpoch: planEpoch,
                currentConnectionEpoch: currentConnectionEpoch
            )
        }
    }

    private func runRouteProbe(
        _ transport: RemoteHostTransport,
        token: UInt64,
        planEpoch: UInt64,
        currentConnectionEpoch: UInt64
    ) async {
        let candidate: RouteProbeCandidate
        do {
            candidate = RouteProbeCandidate(backend: try backendFactory(transport))
        } catch {
            finishRouteProbe(token: token)
            return
        }
        guard routeProbeToken == token, !Task.isCancelled else {
            await candidate.close()
            return
        }
        routeProbeCandidate = candidate
        let backend = candidate.backend

        do {
            let next = try await withTaskCancellationHandler {
                try await backend.bootstrap()
            } onCancel: {
                Task { await candidate.close() }
            }
            try validateProbedIdentity(next, for: transport)
            guard Self.supportsOutput(in: next) else {
                throw NativeRemoteBackendError(
                    result: -1,
                    code: "incompatible_host_protocol",
                    message: "This Host does not advertise remote terminal output."
                )
            }
            guard routeProbeIsCurrent(
                token: token,
                planEpoch: planEpoch,
                currentConnectionEpoch: currentConnectionEpoch
            ) else {
                await candidate.close()
                return
            }
            promoteVerifiedRoute(
                transport,
                candidate: candidate,
                snapshot: next,
                token: token
            )
        } catch {
            await candidate.close()
            finishRouteProbe(token: token)
        }
    }

    private func validateProbedIdentity(
        _ next: RemoteBootstrapSnapshot,
        for transport: RemoteHostTransport
    ) throws {
        if let expectedHostID = transport.expectedHostID,
           next.macID != expectedHostID {
            throw Self.hostIdentityChangedError
        }
        if let pinnedHostID, next.macID != pinnedHostID {
            throw Self.hostIdentityChangedError
        }
    }

    private func routeProbeIsCurrent(
        token: UInt64,
        planEpoch: UInt64,
        currentConnectionEpoch: UInt64
    ) -> Bool {
        !Task.isCancelled
            && routeProbeToken == token
            && pairedPlanEpoch == planEpoch
            && connection?.epoch == currentConnectionEpoch
            && connectionEpoch == currentConnectionEpoch
    }

    private func promoteVerifiedRoute(
        _ transport: RemoteHostTransport,
        candidate: RouteProbeCandidate,
        snapshot next: RemoteBootstrapSnapshot,
        token: UInt64
    ) {
        guard routeProbeToken == token else {
            Task { await candidate.close() }
            return
        }
        let backend = candidate.backend

        routeProbeTask = nil
        routeProbeTarget = nil
        routeProbeCandidate = nil
        connectionEpoch &+= 1
        let epoch = connectionEpoch
        let oldConnection = connection
        let carriedPendingMarkReads = pendingMarkReadSessionIDs
        let carriedAwaitingMarkReads = markReadAwaitingSnapshotClearSessionIDs
        let carriedFailedMarkReads = failedAutomaticMarkReadSessionIDs
        let priorWorker = invalidateConnectionWork()
        // The candidate bootstrap can predate an in-flight mark-read on the
        // retiring route. Keep those latches until the old effect tail has
        // settled and a fresh bootstrap proves the Host's resulting state.
        pendingMarkReadSessionIDs.formUnion(carriedPendingMarkReads)
        markReadAwaitingSnapshotClearSessionIDs.formUnion(carriedAwaitingMarkReads)
        failedAutomaticMarkReadSessionIDs.formUnion(carriedFailedMarkReads)
        connection = nil
        let transferredFits = DesktopFitOwnership()
        let predecessorRetirement = retireForRoutePromotion(
            oldConnection,
            after: priorWorker,
            transferringFitsTo: transferredFits
        )

        // Direct and Link are two routes to one durably pinned Host. Retain
        // the last snapshot, selected Session, and VT panes; only the backend
        // cursor belongs to the replaced connection generation.
        continuityKey = transport.continuityKey
        pinnedHostID = transport.expectedHostID
        paneCursorReady.removeAll()
        connectionRoute = transport.route

        let promoted = ActiveConnection(
            epoch: epoch,
            transport: transport,
            backend: backend,
            effectStartBarrier: predecessorRetirement,
            desktopFits: transferredFits,
            markReadsSuppressedUntilPostBarrier: carriedPendingMarkReads,
            needsPostBarrierRefresh: oldConnection != nil
        )
        connection = promoted
        do {
            try validateIdentity(next, for: promoted)
        } catch {
            // This is defensive: the candidate was checked immediately above
            // and NativeRemoteBackend also enforces its durable expected id.
            connection = nil
            connectionState = .repairRequired(message: error.localizedDescription)
            close(promoted, after: predecessorRetirement)
            return
        }
        publishAcceptedBootstrap(next, for: promoted)
        startRefreshLoop(for: promoted, initiallyAccepted: true)
    }

    private func finishRouteProbe(token: UInt64) {
        guard routeProbeToken == token else { return }
        routeProbeTask = nil
        routeProbeTarget = nil
        routeProbeCandidate = nil
    }

    private func cancelRouteProbe(target: RemoteHostConnectionRoute? = nil) {
        if let target, routeProbeTarget != target { return }
        routeProbeToken &+= 1
        routeProbeTask?.cancel()
        routeProbeTask = nil
        routeProbeTarget = nil
        let candidate = routeProbeCandidate
        routeProbeCandidate = nil
        if let candidate {
            Task { await candidate.close() }
        }
    }

    private static func failureIsRouteReachability(_ error: Error) -> Bool {
        guard let bridgeError = error as? NativeRemoteBackendError else { return false }
        return switch bridgeError.code {
        case "host_connection_launch_failed",
             "host_connection_disconnected",
             "host_connection_timed_out",
             "host_connection_closed":
            true
        default:
            false
        }
    }

    private func validateIdentity(
        _ next: RemoteBootstrapSnapshot,
        for candidate: ActiveConnection
    ) throws {
        if let expectedHostID = candidate.transport.expectedHostID,
           next.macID != expectedHostID {
            throw Self.hostIdentityChangedError
        }
        if let pinnedHostID,
           let reportedHostID = next.macID,
           reportedHostID != pinnedHostID {
            throw Self.hostIdentityChangedError
        }
        if let reportedHostID = next.macID {
            pinnedHostID = reportedHostID
            paneHostKey = "host:\(reportedHostID)"
        } else if let pinnedHostID {
            // A paired/saved Host must never become anonymous.
            guard candidate.transport.expectedHostID == nil else {
                throw Self.hostIdentityChangedError
            }
            paneHostKey = "host:\(pinnedHostID)"
        } else {
            paneHostKey = candidate.transport.continuityKey.paneFallbackKey
        }
    }

    private static var hostIdentityChangedError: NativeRemoteBackendError {
        NativeRemoteBackendError(
            result: -1,
            code: "host_identity_changed",
            message: "Refusing a remote Host whose identity no longer matches the saved Host."
        )
    }

    private func retireForTerminalError(
        _ error: Error,
        matching candidate: ActiveConnection
    ) -> Bool {
        guard let bridgeError = error as? NativeRemoteBackendError else { return false }
        let terminalState: RemoteHostConnectionState
        switch bridgeError.code {
        case "host_identity_changed":
            terminalState = .repairRequired(message: bridgeError.message)
        case "incompatible_host_protocol":
            terminalState = .incompatible(message: bridgeError.message)
        default:
            return false
        }

        guard isCurrent(candidate) else { return true }
        cancelInitialDirectLinkGrace()
        cancelRouteProbe()
        connectionEpoch &+= 1
        let retiringPendingMarkReads = pendingMarkReadSessionIDs
        let retiringAwaitingMarkReads = markReadAwaitingSnapshotClearSessionIDs
        let retiringFailedMarkReads = failedAutomaticMarkReadSessionIDs
        let priorWorker = invalidateConnectionWork()
        connection = nil
        connectionState = terminalState
        if let paneHostKey {
            paneCache.removeHost(paneHostKey)
        }
        paneCursorReady.removeAll()
        let retirement = close(candidate, after: priorWorker) ?? priorWorker
        // Identity/protocol failures leave the Host selected so the user can
        // repair or retry it. Preserve the same continuity barrier as an
        // explicit disconnect: a new backend must not bootstrap or send a fit
        // before this generation's effect tail and compensating clears finish.
        storeOutstandingRetirement(
            for: candidate.transport.continuityKey,
            task: retirement,
            pendingMarkReads: retiringPendingMarkReads,
            awaitingMarkReads: retiringAwaitingMarkReads,
            failedMarkReads: retiringFailedMarkReads
        )
        return true
    }

    @discardableResult
    private func invalidateConnectionWork() -> Task<Void, Never>? {
        cancelInitialDirectLinkGrace()
        var priorWorkers = retiredEffectTails
        retiredEffectTails.removeAll(keepingCapacity: false)
        if let effectWorker {
            priorWorkers.append(effectWorker)
        }
        cancelRefreshLoop()
        stopAllOutputPumps()
        resizeTasks.values.forEach { $0.cancel() }
        resizeTasks.removeAll(keepingCapacity: false)
        recoveryEpoch &+= 1
        effectQueueEpoch &+= 1
        effectWorkerToken &+= 1
        effectWorker = nil
        queuedEffects.removeAll(keepingCapacity: false)
        pendingWriteBytes = 0
        discardDeferredTerminalInput()
        terminalInputOrderingUncertain = false
        writeInFlightQueueEpoch = nil
        pendingMarkReadSessionIDs.removeAll()
        markReadRecoveryBarrier = nil
        markReadsSuppressedUntilRecovery.removeAll()
        markReadAwaitingSnapshotClearSessionIDs.removeAll()
        failedAutomaticMarkReadSessionIDs.removeAll()
        incompleteUTF8InputBySession.removeAll()
        pendingDesktopFitClearSessionIDs.removeAll()
        enqueuedDesktopFitClearSessionIDs.removeAll()
        lastQueuedViewportBySession.removeAll()
        failedFitBySession.removeAll()
        connectionBootstrapped = false
        terminalEffectsEnabled = false
        guard !priorWorkers.isEmpty else { return nil }
        return Task {
            for worker in priorWorkers {
                await worker.value
            }
        }
    }

    @discardableResult
    private func close(
        _ connection: ActiveConnection?,
        after priorWorker: Task<Void, Never>?
    ) -> Task<Void, Never>? {
        guard let connection else { return nil }
        return Task {
            await priorWorker?.value
            await connection.effectStartBarrier?.value
            await Self.clearOwnedDesktopFitsAndClose(connection)
        }
    }

    private func combineTasks(
        _ first: Task<Void, Never>?,
        _ second: Task<Void, Never>?
    ) -> Task<Void, Never>? {
        let tasks = [first, second].compactMap { $0 }
        guard !tasks.isEmpty else { return nil }
        return Task {
            for task in tasks {
                await task.value
            }
        }
    }

    private func storeOutstandingRetirement(
        for key: RemoteHostContinuityKey,
        task: Task<Void, Never>?,
        pendingMarkReads: Set<String>,
        awaitingMarkReads: Set<String>,
        failedMarkReads: Set<String>
    ) {
        let existing = outstandingRetirements.removeValue(forKey: key)
        let combined = combineTasks(existing?.task, task)
        let hasLatches = !pendingMarkReads.isEmpty
            || !awaitingMarkReads.isEmpty
            || !failedMarkReads.isEmpty
            || existing != nil
        guard combined != nil || hasLatches else { return }
        outstandingRetirements[key] = OutstandingRetirement(
            task: combined ?? Task {},
            pendingMarkReads: pendingMarkReads
                .union(existing?.pendingMarkReads ?? []),
            awaitingMarkReads: awaitingMarkReads
                .union(existing?.awaitingMarkReads ?? []),
            failedMarkReads: failedMarkReads
                .union(existing?.failedMarkReads ?? [])
        )
    }

    /// A verified Direct/Link candidate reaches the same durable Host. Never
    /// reconnect a failed retiring route just to clear its fit: transfer that
    /// ownership after its effect tail settles, close it, then compensate on
    /// the working route before any replacement effect starts.
    private func retireForRoutePromotion(
        _ connection: ActiveConnection?,
        after priorWorker: Task<Void, Never>?,
        transferringFitsTo destination: DesktopFitOwnership
    ) -> Task<Void, Never>? {
        guard let connection else { return priorWorker }
        destination.armInheritedClear()
        return Task {
            await priorWorker?.value
            await connection.effectStartBarrier?.value
            for sessionID in connection.desktopFits.snapshot() {
                destination.insert(sessionID)
            }
            await connection.backend.close()
        }
    }

    private nonisolated static func clearOwnedDesktopFitsAndClose(
        _ connection: ActiveConnection
    ) async {
        // Stable sort makes teardown deterministic for tests and diagnostics.
        for sessionID in connection.desktopFits.snapshot().sorted() {
            do {
                _ = try await connection.backend.clearDesktopFit(sessionID: sessionID)
                connection.desktopFits.remove(sessionID)
            } catch {
                // Retirement is best effort. Never replay this clear and
                // never keep an unusable backend alive because cleanup failed.
            }
        }
        await connection.backend.close()
    }

    private func isCurrent(_ candidate: ActiveConnection) -> Bool {
        connection?.epoch == candidate.epoch
            && connectionEpoch == candidate.epoch
    }

    private func adopt(_ incoming: RemoteBootstrapSnapshot) {
        var next = incoming
        if var optimistic = pendingOptimisticCreatedSession {
            if incoming.sessions.contains(where: { $0.id == optimistic.summary.id }) {
                // Host truth has caught up; the ordinary pending-selection path
                // below consumes the intent and replaces the starting summary.
                pendingOptimisticCreatedSession = nil
            } else if optimistic.observationsRemaining > 0 {
                optimistic.observationsRemaining -= 1
                pendingOptimisticCreatedSession = optimistic
                next = Self.snapshot(next, prepending: optimistic.summary)
            } else {
                // A correlated create should publish promptly, but never retain
                // a synthetic row forever if the Host cannot corroborate it.
                pendingOptimisticCreatedSession = nil
                pendingCreatedSelectionID = nil
            }
        }
        if snapshot.map({ !Self.snapshotContentEqual($0, next) }) ?? true {
            snapshot = next
        }

        let validIDs = Set(next.sessions.map(\.id))
        pendingMarkReadSessionIDs.formIntersection(validIDs)
        markReadAwaitingSnapshotClearSessionIDs.formIntersection(validIDs)
        failedAutomaticMarkReadSessionIDs.formIntersection(validIDs)
        for session in next.sessions where !session.unread {
            pendingMarkReadSessionIDs.remove(session.id)
            markReadAwaitingSnapshotClearSessionIDs.remove(session.id)
            failedAutomaticMarkReadSessionIDs.remove(session.id)
        }
        latestViewportByPane = latestViewportByPane.filter { validIDs.contains($0.key.sessionID) }
        // Failed fits are pinned only until the next bootstrap: the Host is
        // alive and talking, so give the fit another chance (a fresh
        // session's host can 404 the first fit while it registers).
        let unpinned = failedFitBySession.keys.filter { validIDs.contains($0) }
        if !unpinned.isEmpty {
            for sessionID in unpinned {
                failedFitBySession.removeValue(forKey: sessionID)
            }
            enqueueLatestPresentedDesktopFits()
        }
        paneCursorReady = paneCursorReady.filter { validIDs.contains($0.key.sessionID) }
        incompleteUTF8InputBySession = incompleteUTF8InputBySession.filter {
            validIDs.contains($0.key)
        }
        if !presentedTerminalSessionIDs.isSubset(of: validIDs) {
            replacePresentedTerminalSessions(
                presentedTerminalSessionIDs.intersection(validIDs)
            )
        }

        let usesDirectSessionDataPlane = connection?.transport.usesDirectSessionDataPlane == true
        if usesDirectSessionDataPlane {
            replacePresentedTerminalSessions([])
        }

        if let pending = pendingCreatedSelectionID, validIDs.contains(pending) {
            if pendingOptimisticCreatedSession?.summary.id != pending {
                pendingCreatedSelectionID = nil
            }
            replacementDefaultSelectionSuppressed = false
            transitionSelectedSession(to: pending)
            publishDirectDataPlaneSelectionIntent(pending)
            prunePaneCache(using: next)
            return
        }

        if let pending = pendingReplacementSelection {
            switch Self.replacementSelectionResolution(
                pending,
                sessions: next.sessions
            ) {
            case let .select(replacementID):
                pendingReplacementSelection = nil
                replacementDefaultSelectionSuppressed = false
                transitionSelectedSession(to: replacementID)
                publishDirectDataPlaneSelectionIntent(replacementID)
                prunePaneCache(using: next)
                return
            case let .wait(updated):
                pendingReplacementSelection = updated
            case .cancel:
                pendingReplacementSelection = nil
            }

            // Preserve a still-valid prior choice while the replacement is
            // unpublished or ambiguous. If it vanished, show no Session
            // rather than silently adopting an unrelated default row.
            if let selectedSessionID, validIDs.contains(selectedSessionID) {
                prunePaneCache(using: next)
                return
            }
            transitionSelectedSession(to: nil)
            prunePaneCache(using: next)
            return
        }

        if let selectedSessionID, validIDs.contains(selectedSessionID) {
            prunePaneCache(using: next)
            return
        }

        // A direct terminal data plane has no Host-owned default selection.
        // Explicit create/restart intents above may select their exact result;
        // ordinary bootstraps otherwise preserve the Controller's choice or
        // settle to nil.
        if usesDirectSessionDataPlane {
            transitionSelectedSession(to: nil)
            prunePaneCache(using: next)
            return
        }

        if replacementDefaultSelectionSuppressed {
            transitionSelectedSession(to: nil)
            prunePaneCache(using: next)
            return
        }

        if let key = connection?.transport.continuityKey.paneFallbackKey,
           let remembered = lastSelectionByScope[key],
           validIDs.contains(remembered) {
            transitionSelectedSession(to: remembered)
        } else {
            transitionSelectedSession(to: Self.defaultSessionID(in: next))
        }
        prunePaneCache(using: next)
    }

    private func transitionSelectedSession(to nextSessionID: String?) {
        guard selectedSessionID != nextSessionID else { return }
        selectedSessionID = nextSessionID
        if let nextSessionID,
           let key = connection?.transport.continuityKey.paneFallbackKey {
            lastSelectionByScope[key] = nextSessionID
        }
        if connection?.transport.usesDirectSessionDataPlane == true {
            replacePresentedTerminalSessions([])
        } else if !presentedSessionsAreViewOwned {
            replacePresentedTerminalSessions(nextSessionID.map { Set([$0]) } ?? [])
        }
    }

    private func publishDirectDataPlaneSelectionIntent(_ sessionID: String?) {
        guard connection?.transport.usesDirectSessionDataPlane == true else { return }
        if directDataPlaneSelectionIntent?.sessionID == sessionID,
           selectedSessionID == sessionID {
            return
        }
        directDataPlaneSelectionSequence &+= 1
        directDataPlaneSelectionIntent = DirectDataPlaneSelectionIntent(
            sequence: directDataPlaneSelectionSequence,
            sessionID: sessionID
        )
    }

    private static func snapshot(
        _ snapshot: RemoteBootstrapSnapshot,
        prepending session: RemoteSessionSummary
    ) -> RemoteBootstrapSnapshot {
        guard !snapshot.sessions.contains(where: { $0.id == session.id }) else {
            return snapshot
        }
        return RemoteBootstrapSnapshot(
            protocolVersion: snapshot.protocolVersion,
            hostProtocol: snapshot.hostProtocol,
            macID: snapshot.macID,
            macName: snapshot.macName,
            folders: snapshot.folders,
            projects: snapshot.projects,
            presets: snapshot.presets,
            workspaceSettings: snapshot.workspaceSettings,
            availableApps: snapshot.availableApps,
            installedApps: snapshot.installedApps,
            openers: snapshot.openers,
            appPresentations: snapshot.appPresentations,
            // Host bootstrap order is newest-first. A correlated create is
            // necessarily newer than the snapshot it raced, so keep that
            // invariant while the real manifest catches up.
            sessions: [session] + snapshot.sessions,
            capturedAtUnixMs: snapshot.capturedAtUnixMs,
            paneGroups: snapshot.paneGroups,
            remoteServerPort: snapshot.remoteServerPort,
            remoteServerCertificateFingerprint:
                snapshot.remoteServerCertificateFingerprint,
            directEndpoint: snapshot.directEndpoint,
            experimentalWorktreesEnabled: snapshot.experimentalWorktreesEnabled,
            proEntitled: snapshot.proEntitled,
            pendingApprovals: snapshot.pendingApprovals,
            hostTintHue: snapshot.hostTintHue,
            hostDeviceKind: snapshot.hostDeviceKind,
            hostDeviceModel: snapshot.hostDeviceModel,
            hostIsolationTier: snapshot.hostIsolationTier,
            hostEnvironment: snapshot.hostEnvironment,
            hostWorkspaces: snapshot.hostWorkspaces
        )
    }

    private func prunePaneCache(using snapshot: RemoteBootstrapSnapshot? = nil) {
        guard let paneHostKey else {
            paneCache.removeAll()
            return
        }
        let current = snapshot ?? self.snapshot
        let liveKeys = Set((current?.sessions ?? []).map {
            RemoteTerminalPaneKey(hostID: paneHostKey, sessionID: $0.id)
        })
        let selectedKey = selectedSessionID.map {
            RemoteTerminalPaneKey(hostID: paneHostKey, sessionID: $0)
        }
        let presentedKeys = Set(presentedTerminalSessionIDs.map {
            RemoteTerminalPaneKey(hostID: paneHostKey, sessionID: $0)
        })
        paneCache.prune(
            keeping: liveKeys,
            selectedKey: selectedKey,
            protectedKeys: presentedKeys
        )
    }

    private func rebindPresentedPanesAndEnsurePumps() {
        guard let connection
        else {
            stopAllOutputPumps()
            return
        }
        for sessionID in presentedTerminalSessionIDs {
            guard let key = paneKey(for: sessionID) else { continue }
            if let pane = paneCache.existingPane(for: key) {
                pane.updateCallbacks(
                    onInput: makeInputHandler(
                        sessionID: sessionID,
                        paneKey: key,
                        connectionEpoch: connection.epoch
                    ),
                    onResize: makeResizeHandler(
                        sessionID: sessionID,
                        paneKey: key,
                        connectionEpoch: connection.epoch
                    )
                )
            }
            ensureOutputPump(for: sessionID)
        }
    }

    private func ensurePresentedOutputPumps() {
        for sessionID in presentedTerminalSessionIDs {
            ensureOutputPump(for: sessionID)
        }
    }

    private func terminalSessionIsPresented(_ sessionID: String) -> Bool {
        presentedTerminalSessionIDs.contains(sessionID)
    }

    private func paneKey(for sessionID: String) -> RemoteTerminalPaneKey? {
        guard let paneHostKey else { return nil }
        return RemoteTerminalPaneKey(hostID: paneHostKey, sessionID: sessionID)
    }

    private func makeInputHandler(
        sessionID: String,
        paneKey: RemoteTerminalPaneKey,
        connectionEpoch: UInt64
    ) -> RemoteTerminalInputHandler {
        { [weak self] data in
            guard let self,
                  self.connection?.epoch == connectionEpoch,
                  self.terminalSessionIsPresented(sessionID),
                  self.paneKey(for: sessionID) == paneKey,
                  self.paneCache.existingPane(for: paneKey) != nil
            else {
                return
            }
            self.sendTerminalInput(data, to: sessionID)
        }
    }

    private func makeResizeHandler(
        sessionID: String,
        paneKey: RemoteTerminalPaneKey,
        connectionEpoch: UInt64
    ) -> RemoteTerminalResizeHandler {
        { [weak self] viewport in
            // Deliberately NOT gated on presentation: the surface's very
            // first resize on a fresh scope can fire before the runtime has
            // marked the Session presented (the presented set intersects a
            // snapshot that may still be bootstrapping). The fit must still
            // be RECORDED so presentation replays it — dropping it here left
            // the Host PTY at its stale grid until a manual window resize
            // fired the next (only) resize callback.
            guard let self else { return }
            guard self.connection?.epoch == connectionEpoch,
                  self.paneKey(for: sessionID) == paneKey,
                  self.paneCache.existingPane(for: paneKey) != nil
            else {
                return
            }
            self.scheduleDesktopFit(
                sessionID: sessionID,
                paneKey: paneKey,
                connectionEpoch: connectionEpoch,
                viewport: viewport
            )
        }
    }

    private func scheduleDesktopFit(
        sessionID: String,
        paneKey: RemoteTerminalPaneKey,
        connectionEpoch: UInt64,
        viewport: RemoteTerminalViewport
    ) {
        guard viewport.columns > 0, viewport.rows > 0 else { return }
        resizeTasks.removeValue(forKey: paneKey)?.cancel()
        resizeTasks[paneKey] = Task { [weak self] in
            do {
                try await Task.sleep(nanoseconds: self?.resizeDebounceNanoseconds ?? 0)
            } catch {
                return
            }
            guard let self,
                  self.connection?.epoch == connectionEpoch,
                  self.paneKey(for: sessionID) == paneKey
            else {
                return
            }
            let requested = RequestedDesktopFit(
                columns: UInt16(clamping: viewport.columns),
                rows: UInt16(clamping: viewport.rows)
            )
            // Record BEFORE the presentation gate: a not-yet-presented fit
            // is replayed by enqueueLatestPresentedDesktopFits() the moment
            // the Session joins the presented set.
            self.latestViewportByPane[paneKey] = requested
            guard self.terminalSessionIsPresented(sessionID) else { return }
            self.enqueueDesktopFitIfNeeded(
                sessionID: sessionID,
                columns: requested.columns,
                rows: requested.rows,
                viewport: requested
            )
        }
    }

    private func ensureOutputPump(for sessionID: String) {
        guard connectionBootstrapped,
              Self.supportsOutput(in: snapshot),
              let connection,
              terminalSessionIsPresented(sessionID),
              let paneKey = paneKey(for: sessionID)
        else {
            return
        }
        guard let pane = paneCache.existingPane(for: paneKey) else { return }
        let paneIdentity = ObjectIdentifier(pane)

        if let existing = outputPumpIdentities[paneKey],
           existing.connectionEpoch == connection.epoch,
           existing.paneKey == paneKey,
           existing.paneIdentity == paneIdentity,
           outputTasks[paneKey] != nil {
            return
        }

        stopOutputPump(for: paneKey)
        outputPumpToken &+= 1
        let identity = OutputPumpIdentity(
            token: outputPumpToken,
            connectionEpoch: connection.epoch,
            paneKey: paneKey,
            paneIdentity: paneIdentity
        )
        outputPumpIdentities[paneKey] = identity
        outputTasks[paneKey] = Task { [weak self] in
            guard let self else { return }
            await self.runOutputPump(connection: connection, identity: identity)
            self.finishOutputPump(identity)
        }
    }

    private func runOutputPump(
        connection: ActiveConnection,
        identity: OutputPumpIdentity
    ) async {
        guard await prepareOutputCursor(connection: connection, identity: identity) else {
            return
        }

        while !Task.isCancelled {
            guard outputPumpIsCurrent(identity, connection: connection),
                  let pane = paneCache.existingPane(for: identity.paneKey)
            else {
                return
            }

            guard paneAttachmentProbe(pane) else {
                if !(await sleepOutputIdle()) { return }
                continue
            }

            let page: NativeRemoteOutputPage
            do {
                // The Host contract caps pages at 200 KiB. Keep a little
                // headroom for adapters while still amortizing long output.
                page = try await connection.backend.pollOutput(
                    sessionID: identity.paneKey.sessionID,
                    limit: 192 * 1024,
                    waitMilliseconds: 1_000
                )
            } catch is CancellationError {
                return
            } catch {
                guard outputPumpIsCurrent(identity, connection: connection) else {
                    return
                }
                if Self.outputFailureIsTransient(error) {
                    if !(await sleepOutputIdle()) { return }
                    continue
                }
                handleTransportFailure(error, matching: connection)
                return
            }

            guard outputPumpIsCurrent(identity, connection: connection),
                  page.metadata.sessionID == identity.paneKey.sessionID,
                  let currentPane = paneCache.existingPane(for: identity.paneKey),
                  currentPane === pane
            else {
                await connection.backend.discardOutput(page)
                return
            }
            // SwiftUI may briefly detach a retained NSView while moving it
            // between pane slots. Discard this uncommitted page so the Host
            // cursor stays put, but keep the presented pane's pump alive for
            // the replacement mount.
            guard paneAttachmentProbe(currentPane) else {
                await connection.backend.discardOutput(page)
                guard outputPumpIsCurrent(identity, connection: connection),
                      await sleepOutputIdle()
                else { return }
                continue
            }

            let accepted = paneOutputFeeder(
                currentPane,
                page.bytes,
                page.metadata.resetBeforeFeed
            )
            guard accepted else {
                await connection.backend.discardOutput(page)
                if !outputPumpIsCurrent(identity, connection: connection) {
                    return
                }
                if !(await sleepOutputIdle()) { return }
                continue
            }

            // Acceptance and commit are intentionally adjacent. Once the VT
            // accepted bytes, committing is correct even if selection changes
            // while this await is in flight; not committing would replay the
            // same bytes into a retained pane on the next selection.
            do {
                try await connection.backend.commitOutput(page)
            } catch {
                // Bytes already entered this VT, but the backend cursor may
                // still point before them. Recovery must request a fresh tail
                // and atomically replace the pane instead of appending a
                // duplicate control sequence.
                paneCursorReady.removeValue(forKey: identity.paneKey)
                guard outputPumpIsCurrent(identity, connection: connection) else {
                    // A pane can detach and reattach while commit is in
                    // flight. In that race the replacement pump may have
                    // observed the cursor as ready immediately before this
                    // failure revoked it. Restart that exact current pane so
                    // prepareOutputCursor cannot continue polling from the
                    // now-uncertain cursor.
                    if isCurrent(connection),
                       terminalSessionIsPresented(identity.paneKey.sessionID),
                       paneKey(for: identity.paneKey.sessionID) == identity.paneKey {
                        stopOutputPump(for: identity.paneKey)
                        ensureOutputPump(for: identity.paneKey.sessionID)
                    }
                    return
                }
                handleTransportFailure(error, matching: connection)
                return
            }

            // The receipt belongs only to the captured generation. There is
            // no state to publish, but this guard makes that rule explicit.
            guard outputPumpIsCurrent(identity, connection: connection) else {
                return
            }
        }
    }

    private func prepareOutputCursor(
        connection: ActiveConnection,
        identity: OutputPumpIdentity
    ) async -> Bool {
        guard outputPumpIsCurrent(identity, connection: connection),
              let pane = paneCache.existingPane(for: identity.paneKey),
              ObjectIdentifier(pane) == identity.paneIdentity
        else { return false }

        let cursorIdentity = PaneCursorIdentity(
            connectionEpoch: connection.epoch,
            paneIdentity: identity.paneIdentity
        )
        if paneCursorReady[identity.paneKey] == cursorIdentity {
            return true
        }

        do {
            try await connection.backend.resetOutput(
                sessionID: identity.paneKey.sessionID
            )
        } catch is CancellationError {
            return false
        } catch {
            guard outputPumpIsCurrent(identity, connection: connection) else {
                return false
            }
            handleTransportFailure(error, matching: connection)
            return false
        }

        guard outputPumpIsCurrent(identity, connection: connection),
              let currentPane = paneCache.existingPane(for: identity.paneKey),
              ObjectIdentifier(currentPane) == identity.paneIdentity
        else { return false }
        paneCursorReady[identity.paneKey] = cursorIdentity
        return true
    }

    private func outputPumpIsCurrent(
        _ identity: OutputPumpIdentity,
        connection: ActiveConnection
    ) -> Bool {
        !Task.isCancelled
            && self.connection?.epoch == connection.epoch
            && connectionEpoch == connection.epoch
            && outputPumpIdentities[identity.paneKey] == identity
            && paneCache.existingPane(for: identity.paneKey).map(ObjectIdentifier.init)
                == identity.paneIdentity
            && terminalSessionIsPresented(identity.paneKey.sessionID)
            && paneKey(for: identity.paneKey.sessionID) == identity.paneKey
            && connectionBootstrapped
    }

    private func finishOutputPump(_ identity: OutputPumpIdentity) {
        guard outputPumpIdentities[identity.paneKey] == identity else { return }
        outputTasks.removeValue(forKey: identity.paneKey)
        outputPumpIdentities.removeValue(forKey: identity.paneKey)
    }

    private func stopOutputPump(for paneKey: RemoteTerminalPaneKey) {
        outputPumpIdentities.removeValue(forKey: paneKey)
        outputTasks.removeValue(forKey: paneKey)?.cancel()
    }

    private func stopAllOutputPumps() {
        outputPumpIdentities.removeAll(keepingCapacity: false)
        let tasks = Array(outputTasks.values)
        outputTasks.removeAll(keepingCapacity: false)
        tasks.forEach { $0.cancel() }
    }

    private func sleepOutputIdle() async -> Bool {
        do {
            try await Task.sleep(nanoseconds: outputIdleIntervalNanoseconds)
            return !Task.isCancelled
        } catch {
            return false
        }
    }

    private func handleTransportFailure(
        _ error: Error,
        matching candidate: ActiveConnection
    ) {
        guard isCurrent(candidate) else { return }
        if retireForTerminalError(error, matching: candidate) {
            return
        }
        disableRemoteIO(error, matching: candidate)
    }

    private static func outputFailureIsTransient(_ error: Error) -> Bool {
        (error as? NativeRemoteBackendError)?.code == "output_page_pending"
    }

    @discardableResult
    private func enqueueEffect(_ effect: RemoteEffect) -> Bool {
        guard connectionBootstrapped,
              let connection,
              Self.effectSupported(effect, by: snapshot),
              Self.effectDoesNotRequireSelection(effect)
                  || terminalSessionIsPresented(effect.sessionID)
        else { return false }

        if case let .clearFit(sessionID) = effect {
            guard connection.desktopFits.snapshot().contains(sessionID) else {
                return false
            }
        } else if !Self.supportsOutput(in: snapshot) {
            return false
        }

        if case .write = effect, !terminalEffectsEnabled { return false }
        if case .clearFit = effect {
            // An owned Session may disappear from the next bootstrap before
            // its compensating clear is sent.
        } else if snapshot?.sessions.contains(where: { $0.id == effect.sessionID }) != true {
            return false
        }

        switch effect {
        case let .write(sessionID, data):
            guard pendingWriteBytes <= Self.maximumPendingTerminalInputBytes - data.count else {
                disableRemoteIO(
                    Self.inputBackpressureError,
                    matching: connection,
                    preservesUnsentInput: false
                )
                return false
            }
            pendingWriteBytes += data.count
            appendWriteBatches(sessionID: sessionID, data: data)
        case let .markRead(sessionID):
            guard pendingMarkReadSessionIDs.insert(sessionID).inserted else { return false }
            queuedEffects.append(QueuedEffect(effect: effect))
        case let .fit(sessionID, columns, rows):
            if case let .fit(lastID, _, _)? = queuedEffects.last?.effect,
               lastID == sessionID {
                queuedEffects[queuedEffects.count - 1].effect = .fit(
                    sessionID: sessionID,
                    columns: columns,
                    rows: rows
                )
            } else {
                queuedEffects.append(QueuedEffect(effect: effect))
            }
        case .clearFit:
            queuedEffects.append(QueuedEffect(effect: effect))
        }

        startEffectWorkerIfNeeded(connection)
        return true
    }

    /// The current Rust wire effect is UTF-8 text even though Ghostty's input
    /// callback is byte-oriented. Preserve a scalar split across callbacks
    /// (at most three bytes) rather than dispatching an invalid half-scalar.
    private func prepareTerminalInput(_ data: Data, sessionID: String) -> Data? {
        var combined = incompleteUTF8InputBySession.removeValue(forKey: sessionID) ?? Data()
        combined.append(data)
        if String(data: combined, encoding: .utf8) != nil {
            return combined
        }

        for suffixCount in 1...min(3, combined.count) {
            let split = combined.index(combined.endIndex, offsetBy: -suffixCount)
            let prefix = Data(combined[..<split])
            let suffix = Data(combined[split...])
            if String(data: prefix, encoding: .utf8) != nil,
               Self.isIncompleteUTF8ScalarPrefix(suffix) {
                incompleteUTF8InputBySession[sessionID] = suffix
                return prefix
            }
        }

        if let connection {
            disableRemoteIO(Self.invalidTerminalInputError, matching: connection)
        }
        return nil
    }

    private static func isIncompleteUTF8ScalarPrefix(_ bytes: Data) -> Bool {
        guard let first = bytes.first else { return false }
        let expectedLength: Int
        switch first {
        case 0xC2...0xDF: expectedLength = 2
        case 0xE0...0xEF: expectedLength = 3
        case 0xF0...0xF4: expectedLength = 4
        default: return false
        }
        guard bytes.count < expectedLength else { return false }
        let values = Array(bytes)
        if values.count >= 2 {
            let second = values[1]
            let validSecond: Bool
            switch first {
            case 0xE0: validSecond = (0xA0...0xBF).contains(second)
            case 0xED: validSecond = (0x80...0x9F).contains(second)
            case 0xF0: validSecond = (0x90...0xBF).contains(second)
            case 0xF4: validSecond = (0x80...0x8F).contains(second)
            default: validSecond = (0x80...0xBF).contains(second)
            }
            guard validSecond else { return false }
        }
        return values.dropFirst(2).allSatisfy { (0x80...0xBF).contains($0) }
    }

    private func appendWriteBatches(sessionID: String, data: Data) {
        var offset = data.startIndex
        while offset < data.endIndex {
            if case let .write(lastID, existing)? = queuedEffects.last?.effect,
               lastID == sessionID,
               existing.count < Self.maximumTerminalWriteBytes {
                let capacity = Self.maximumTerminalWriteBytes - existing.count
                let end = Self.utf8SafeBatchEnd(in: data, from: offset, limit: capacity)
                // The remaining capacity may be smaller than the next
                // multi-byte scalar. Leave it unused and start a new batch.
                if end == offset {
                    let freshEnd = Self.utf8SafeBatchEnd(
                        in: data,
                        from: offset,
                        limit: Self.maximumTerminalWriteBytes
                    )
                    let boundedEnd = freshEnd == offset
                        ? data.index(
                            offset,
                            offsetBy: min(
                                Self.maximumTerminalWriteBytes,
                                data.distance(from: offset, to: data.endIndex)
                            )
                        )
                        : freshEnd
                    queuedEffects.append(QueuedEffect(effect: .write(
                        sessionID: sessionID,
                        data: Data(data[offset..<boundedEnd])
                    )))
                    offset = boundedEnd
                    continue
                }
                var combined = existing
                combined.append(data[offset..<end])
                queuedEffects[queuedEffects.count - 1].effect = .write(
                    sessionID: sessionID,
                    data: combined
                )
                offset = end
                continue
            }

            let end = Self.utf8SafeBatchEnd(
                in: data,
                from: offset,
                limit: Self.maximumTerminalWriteBytes
            )
            let boundedEnd = end == offset
                ? data.index(
                    offset,
                    offsetBy: min(
                        Self.maximumTerminalWriteBytes,
                        data.distance(from: offset, to: data.endIndex)
                    )
                )
                : end
            queuedEffects.append(QueuedEffect(effect: .write(
                sessionID: sessionID,
                data: Data(data[offset..<boundedEnd])
            )))
            offset = boundedEnd
        }
    }

    private static func utf8SafeBatchEnd(
        in data: Data,
        from start: Data.Index,
        limit: Int
    ) -> Data.Index {
        var end = data.index(start, offsetBy: min(limit, data.distance(from: start, to: data.endIndex)))
        if end < data.endIndex {
            while end > start, data[end] & 0b1100_0000 == 0b1000_0000 {
                end = data.index(before: end)
            }
        }
        return end
    }

    private func startEffectWorkerIfNeeded(_ connection: ActiveConnection) {
        guard effectWorker == nil,
              !queuedEffects.isEmpty || connection.desktopFits.hasInheritedClearPending()
        else { return }
        effectWorkerToken &+= 1
        let token = effectWorkerToken
        let queueEpoch = effectQueueEpoch
        effectWorker = Task { [weak self] in
            guard let self else { return }
            await self.runEffectWorker(
                connection: connection,
                queueEpoch: queueEpoch,
                token: token
            )
            self.finishEffectWorker(token: token)
        }
    }

    private func runEffectWorker(
        connection: ActiveConnection,
        queueEpoch: UInt64,
        token: UInt64
    ) async {
        // A previous connection to the same Host may still own a global
        // desktop fit. Its ordered clear must land before this generation can
        // apply a replacement fit (or any later UI effect).
        await connection.effectStartBarrier?.value
        guard effectWorkerIsCurrent(connection, queueEpoch: queueEpoch, token: token) else {
            return
        }
        let inheritedFits = connection.desktopFits.claimInheritedFitsForClear()
        if !inheritedFits.isEmpty {
            let alreadyQueued = Set(queuedEffects.compactMap { queued -> String? in
                guard case let .clearFit(sessionID) = queued.effect else { return nil }
                return sessionID
            })
            let compensatingClears = inheritedFits
                .subtracting(alreadyQueued)
                .sorted()
                .map { QueuedEffect(effect: .clearFit(sessionID: $0)) }
            queuedEffects.insert(contentsOf: compensatingClears, at: 0)
        }
        while effectWorkerIsCurrent(connection, queueEpoch: queueEpoch, token: token),
              !queuedEffects.isEmpty {
            let queued = queuedEffects.removeFirst()
            if case .write = queued.effect {
                writeInFlightQueueEpoch = queueEpoch
            }
            do {
                _ = try await queued.effect.perform(on: connection.backend)
                if writeInFlightQueueEpoch == queueEpoch {
                    writeInFlightQueueEpoch = nil
                }
                if case let .fit(sessionID, _, _) = queued.effect {
                    // Enqueue-time ownership is conservative for ambiguous
                    // outcomes; success records it again in case a preceding
                    // compensating clear removed the inherited ownership.
                    connection.desktopFits.insert(sessionID)
                }
                if case let .clearFit(sessionID) = queued.effect {
                    let hasReplacementFit = queuedEffects.contains { later in
                        guard case let .fit(laterSessionID, _, _) = later.effect else {
                            return false
                        }
                        return laterSessionID == sessionID
                    }
                    if !hasReplacementFit {
                        connection.desktopFits.remove(sessionID)
                    }
                }
                guard effectWorkerIsCurrent(
                    connection,
                    queueEpoch: queueEpoch,
                    token: token
                ) else { return }
                completeEffect(queued.effect)
            } catch {
                if writeInFlightQueueEpoch == queueEpoch {
                    writeInFlightQueueEpoch = nil
                }
                // A failed clear must never be replayed during retirement,
                // unless the Host correlated a semantic NotApplied response.
                // In that one case the inherited fit is proven to remain and
                // its ownership must survive a later replacement fit/close.
                if case let .clearFit(sessionID) = queued.effect {
                    if (error as? NativeRemoteBackendError)?
                        .effectCanContinueOnCurrentGeneration != true {
                        connection.desktopFits.remove(sessionID)
                    }
                }
                guard isCurrent(connection), effectQueueEpoch == queueEpoch else { return }
                if handleEffectFailure(
                    error,
                    queuedEffect: queued,
                    matching: connection
                ) {
                    continue
                }
                return
            }
        }
    }

    private func effectWorkerIsCurrent(
        _ candidate: ActiveConnection,
        queueEpoch: UInt64,
        token: UInt64
    ) -> Bool {
        isCurrent(candidate)
            && connectionBootstrapped
            && effectQueueEpoch == queueEpoch
            && effectWorkerToken == token
    }

    private func finishEffectWorker(token: UInt64) {
        guard effectWorkerToken == token else { return }
        effectWorker = nil
        if let connection,
           !queuedEffects.isEmpty || connection.desktopFits.hasInheritedClearPending() {
            startEffectWorkerIfNeeded(connection)
        }
    }

    private func completeEffect(_ effect: RemoteEffect) {
        switch effect {
        case let .write(_, data):
            pendingWriteBytes = max(0, pendingWriteBytes - data.count)
        case .fit:
            break
        case let .clearFit(sessionID):
            pendingDesktopFitClearSessionIDs.remove(sessionID)
            enqueuedDesktopFitClearSessionIDs.remove(sessionID)
            lastQueuedViewportBySession.removeValue(forKey: sessionID)
        case let .markRead(sessionID):
            pendingMarkReadSessionIDs.remove(sessionID)
            markReadAwaitingSnapshotClearSessionIDs.insert(sessionID)
        }
    }

    /// Returns true only when the Host proved the effect was not applied and
    /// the serial queue may safely continue on the same generation.
    private func handleEffectFailure(
        _ error: Error,
        queuedEffect: QueuedEffect,
        matching candidate: ActiveConnection
    ) -> Bool {
        guard isCurrent(candidate) else { return false }
        let effect = queuedEffect.effect
        if case let .clearFit(sessionID) = effect {
            pendingDesktopFitClearSessionIDs.remove(sessionID)
            enqueuedDesktopFitClearSessionIDs.remove(sessionID)
            lastQueuedViewportBySession.removeValue(forKey: sessionID)
        }
        if case let .markRead(sessionID) = effect {
            pendingMarkReadSessionIDs.remove(sessionID)
            failedAutomaticMarkReadSessionIDs.insert(sessionID)
        }
        if case let .fit(sessionID, columns, rows) = effect {
            // The fit was NOT applied: forget the queued-viewport dedupe or
            // the eventual retry would be silently swallowed.
            lastQueuedViewportBySession.removeValue(forKey: sessionID)
            failedFitBySession[sessionID] = RequestedDesktopFit(
                columns: columns,
                rows: rows
            )
        }
        if (error as? NativeRemoteBackendError)?.effectCanContinueOnCurrentGeneration == true {
            if case .write = effect {
                guard queuedEffect.notAppliedRetryCount == 0 else {
                    // A missing prefix must never be skipped while later
                    // terminal bytes continue. One correlated NotApplied
                    // retry is safe; a second rejection drops the tail and
                    // requires a fresh bootstrap instead of corrupting order.
                    terminalInputOrderingUncertain = true
                    disableRemoteIO(error, matching: candidate)
                    return false
                }
                var retry = queuedEffect
                retry.notAppliedRetryCount = 1
                queuedEffects.insert(retry, at: 0)
                return true
            }
            // Keep fit ownership conservative. A rejected replacement may be
            // sitting on top of an earlier successful fit; retirement must
            // still clear that Host-global fit before closing or switching.
            return true
        }
        if retireForTerminalError(error, matching: candidate) { return false }
        // An unknown outcome stops the serial queue. Nothing behind the
        // failed call is replayed on this or a fallback transport.
        if case .write = effect {
            terminalInputOrderingUncertain = true
        }
        disableRemoteIO(error, matching: candidate)
        return false
    }

    private func disableRemoteIO(
        _ error: Error,
        matching candidate: ActiveConnection,
        preservesUnsentInput: Bool = true
    ) {
        guard isCurrent(candidate) else { return }
        cancelInitialDirectLinkGrace()
        if !Self.failureIsRouteReachability(error) {
            // A speculative grace probe must never bypass an authentication,
            // identity, capability, or semantic failure on the preferred
            // Direct route.
            cancelRouteProbe()
        }
        let markReadsFromRetiringTail = pendingMarkReadSessionIDs
        recoveryEpoch &+= 1
        connectionBootstrapped = false
        terminalEffectsEnabled = false
        stopAllOutputPumps()
        resizeTasks.values.forEach { $0.cancel() }
        resizeTasks.removeAll(keepingCapacity: false)
        effectQueueEpoch &+= 1
        effectWorkerToken &+= 1
        if let effectWorker {
            retiredEffectTails.append(effectWorker)
        }
        if !markReadsFromRetiringTail.isEmpty {
            let priorBarrier = markReadRecoveryBarrier
            let tails = retiredEffectTails
            markReadsSuppressedUntilRecovery.formUnion(markReadsFromRetiringTail)
            markReadRecoveryBarrier = Task {
                await priorBarrier?.value
                for tail in tails {
                    await tail.value
                }
            }
        }
        effectWorker = nil
        // Queued writes were never dispatched. With no write in flight and
        // no earlier uncertainty they are provably unreceived, so hold them
        // (in order) for the next accepted bootstrap instead of losing the
        // user's keystrokes. Backpressure is the one deliberate drop.
        if writeInFlightQueueEpoch != nil || terminalInputOrderingUncertain
            || !preservesUnsentInput {
            terminalInputOrderingUncertain = true
            discardDeferredTerminalInput()
        } else {
            for queued in queuedEffects {
                if case let .write(sessionID, data) = queued.effect {
                    deferTerminalInput(sessionID: sessionID, data: data)
                }
            }
        }
        writeInFlightQueueEpoch = nil
        queuedEffects.removeAll(keepingCapacity: false)
        pendingWriteBytes = 0
        pendingMarkReadSessionIDs.removeAll()
        incompleteUTF8InputBySession.removeAll()
        enqueuedDesktopFitClearSessionIDs.removeAll()
        lastQueuedViewportBySession.removeAll()
        scheduleFallbackIfPossible(for: error, matching: candidate)
        pendingMarkReadSessionIDs.formUnion(markReadsFromRetiringTail)
        connectionState = snapshot == nil
            ? (routeProbeTask == nil
                ? .failed(message: error.localizedDescription)
                : .connecting)
            : .reconnecting(message: error.localizedDescription)
    }

    private func enqueueDesktopFitIfNeeded(
        sessionID: String,
        columns: UInt16,
        rows: UInt16,
        viewport: RequestedDesktopFit
    ) {
        // Presentation settle gate: hold the fit until the Session has been
        // presented for a beat, so fly-by switches never resize the Host
        // PTY. The held fit re-runs with the latest geometry once settled.
        let settleInterval = fitSettleInterval
        if settleInterval > 0, let presentedAt = presentedAtBySession[sessionID] {
            let elapsed = Date().timeIntervalSince(presentedAt)
            if elapsed < settleInterval {
                settleFitTasks[sessionID]?.cancel()
                settleFitTasks[sessionID] = Task { [weak self] in
                    let remaining = settleInterval - elapsed
                    try? await Task.sleep(
                        nanoseconds: UInt64(max(0, remaining) * 1_000_000_000)
                    )
                    guard let self, !Task.isCancelled else { return }
                    self.settleFitTasks.removeValue(forKey: sessionID)
                    guard self.terminalSessionIsPresented(sessionID) else { return }
                    // Re-derive from the pane's current grid: the geometry
                    // may have changed while the fit was held.
                    if let paneKey = self.paneKey(for: sessionID),
                       let pane = self.paneCache.existingPane(for: paneKey),
                       let grid = pane.currentGrid() {
                        let latest = RequestedDesktopFit(
                            columns: UInt16(clamping: grid.columns),
                            rows: UInt16(clamping: grid.rows)
                        )
                        self.latestViewportByPane[paneKey] = latest
                        self.enqueueDesktopFitIfNeeded(
                            sessionID: sessionID,
                            columns: latest.columns,
                            rows: latest.rows,
                            viewport: latest
                        )
                    } else {
                        self.enqueueDesktopFitIfNeeded(
                            sessionID: sessionID,
                            columns: columns,
                            rows: rows,
                            viewport: viewport
                        )
                    }
                }
                return
            }
        }
        if failedFitBySession[sessionID] == viewport { return }
        failedFitBySession.removeValue(forKey: sessionID)
        guard lastQueuedViewportBySession[sessionID] != viewport,
              let connection
        else { return }
        if enqueueEffect(.fit(
            sessionID: sessionID,
            columns: columns,
            rows: rows
        )) {
            lastQueuedViewportBySession[sessionID] = viewport
            connection.desktopFits.insert(sessionID)
        }
    }

    private func requestDesktopFitClear(_ sessionID: String) {
        guard let connection,
              connection.desktopFits.snapshot().contains(sessionID)
        else { return }
        pendingDesktopFitClearSessionIDs.insert(sessionID)
        tryEnqueueDesktopFitClear(sessionID)
    }

    private func tryEnqueueDesktopFitClear(_ sessionID: String) {
        guard pendingDesktopFitClearSessionIDs.contains(sessionID),
              !enqueuedDesktopFitClearSessionIDs.contains(sessionID)
        else { return }
        if enqueueEffect(.clearFit(sessionID: sessionID)) {
            enqueuedDesktopFitClearSessionIDs.insert(sessionID)
        }
    }

    private func enqueuePendingDesktopFitClears() {
        for sessionID in pendingDesktopFitClearSessionIDs.sorted() {
            tryEnqueueDesktopFitClear(sessionID)
        }
    }

    /// The fit pipeline has several legitimate drop points (settle gating,
    /// Host 404s while a fresh session registers, generation churn). Rather
    /// than trusting every path to never lose a fit, presented sessions'
    /// fits are re-asserted from their panes' LIVE grids on a slow loop. A
    /// fit the Host already applied is a same-size no-op (no SIGWINCH, no
    /// TUI redraw), so steady state costs one tiny effect per interval —
    /// and any drift, whatever dropped it, self-heals within one tick.
    private func updateFitReassertLoop() {
        if presentedTerminalSessionIDs.isEmpty {
            fitReassertTask?.cancel()
            fitReassertTask = nil
            return
        }
        guard fitReassertTask == nil else { return }
        fitReassertTask = Task { [weak self] in
            var tick = 0
            while !Task.isCancelled {
                try? await Task.sleep(nanoseconds: 5_000_000_000)
                guard let self, !Task.isCancelled else { return }
                guard !self.presentedTerminalSessionIDs.isEmpty else {
                    self.fitReassertTask = nil
                    return
                }
                tick += 1
                // Gentle ticks respect the queued-viewport dedupe (steady
                // state sends nothing); every 6th tick forces a re-send so
                // even a silently-lost applied fit converges within 30s.
                self.reassertPresentedDesktopFits(force: tick % 6 == 0)
            }
        }
    }

    private func reassertPresentedDesktopFits(force: Bool) {
        for sessionID in presentedTerminalSessionIDs {
            guard let paneKey = paneKey(for: sessionID),
                  let pane = paneCache.existingPane(for: paneKey),
                  pane.window != nil,
                  let grid = pane.currentGrid()
            else { continue }
            let viewport = RequestedDesktopFit(
                columns: UInt16(clamping: grid.columns),
                rows: UInt16(clamping: grid.rows)
            )
            latestViewportByPane[paneKey] = viewport
            if force {
                lastQueuedViewportBySession.removeValue(forKey: sessionID)
                failedFitBySession.removeValue(forKey: sessionID)
            }
            enqueueDesktopFitIfNeeded(
                sessionID: sessionID,
                columns: viewport.columns,
                rows: viewport.rows,
                viewport: viewport
            )
        }
    }

    private func enqueueLatestPresentedDesktopFits() {
        for sessionID in presentedTerminalSessionIDs {
            guard let paneKey = paneKey(for: sessionID) else { continue }
            // A live pane's surface is the truth: overwrite whatever was
            // recorded (a viewport captured during attach churn can be
            // stale) with the grid the surface actually shows right now.
            if let pane = paneCache.existingPane(for: paneKey),
               pane.window != nil,
               let grid = pane.currentGrid() {
                latestViewportByPane[paneKey] = RequestedDesktopFit(
                    columns: UInt16(clamping: grid.columns),
                    rows: UInt16(clamping: grid.rows)
                )
            }
            guard let viewport = latestViewportByPane[paneKey] else { continue }
            enqueueDesktopFitIfNeeded(
                sessionID: sessionID,
                columns: viewport.columns,
                rows: viewport.rows,
                viewport: viewport
            )
        }
    }

    private func requestMarkReadIfNeeded(_ sessionID: String) {
        guard snapshot?.sessions.first(where: { $0.id == sessionID })?.unread == true,
              !markReadAwaitingSnapshotClearSessionIDs.contains(sessionID),
              !failedAutomaticMarkReadSessionIDs.contains(sessionID)
        else { return }
        markRead(sessionID: sessionID)
    }

    private static func effectDoesNotRequireSelection(_ effect: RemoteEffect) -> Bool {
        switch effect {
        case .clearFit, .markRead: true
        case .write, .fit: false
        }
    }

    private static func effectSupported(
        _ effect: RemoteEffect,
        by snapshot: RemoteBootstrapSnapshot?
    ) -> Bool {
        let capability: String
        switch effect {
        case .write: capability = "session.input.write"
        case .fit, .clearFit: capability = "session.resize_desktop"
        case .markRead: capability = "session.mark_read"
        }
        return snapshot?.hostProtocol?.supports(capability) == true
    }

    private static func supportsInput(in snapshot: RemoteBootstrapSnapshot) -> Bool {
        snapshot.hostProtocol?.supports("session.input.write") == true
    }

    private static func supportsOutput(in snapshot: RemoteBootstrapSnapshot?) -> Bool {
        snapshot?.hostProtocol?.supports("session.output.read") == true
    }

    private static var inputBackpressureError: NativeRemoteBackendError {
        NativeRemoteBackendError(
            result: -1,
            code: "remote_input_backpressure",
            message: "Remote input paused because this Host could not keep up with the pending paste. Reconnect before sending it again.",
            kind: "notApplied",
            operation: "terminal write"
        )
    }

    private static var invalidTerminalInputError: NativeRemoteBackendError {
        NativeRemoteBackendError(
            result: -1,
            code: "invalid_terminal_input_utf8",
            message: "Remote input paused because this Host transport cannot encode those terminal bytes safely.",
            kind: "notApplied",
            operation: "terminal write"
        )
    }

    static func defaultSessionID(in snapshot: RemoteBootstrapSnapshot) -> String? {
        let sessions = snapshot.sessions.filter { !$0.archived }
        return sessions.first(where: { $0.activity == .blocked })?.id
            ?? sessions.first(where: { $0.status == .running })?.id
            ?? sessions.first?.id
    }

    /// Resolve a legacy restart receipt to exactly one replacement row. A
    /// valid candidate must be new relative to the pre-effect baseline, must
    /// preserve the stable project/creation/worktree identity, and must belong
    /// to the same runtime command family. The old row must also have vanished,
    /// proving this is a replacement rather than an unrelated concurrent
    /// launch. Multiple candidates permanently cancel automatic selection.
    static func replacementSelectionResolution(
        _ intent: ReplacementSelectionIntent,
        sessions: [RemoteSessionSummary]
    ) -> ReplacementSelectionResolution {
        let sourceStillExists = sessions.contains { $0.id == intent.sourceSessionID }
        let candidates = sessions.filter { session in
            guard session.id != intent.sourceSessionID,
                  !intent.baselineSessionIDs.contains(session.id),
                  session.projectID == intent.projectID,
                  session.createdAtUnixMs == intent.createdAtUnixMs,
                  session.status == .running,
                  !session.archived,
                  session.worktreePath == intent.worktreePath,
                  session.worktreeBranch == intent.worktreeBranch
            else { return false }
            guard let expectedRuntimeID = intent.runtimeID else { return true }
            let candidateRuntimeID = session.providerID
                ?? SetupTool.detect(in: session.command)?.id
            return candidateRuntimeID == expectedRuntimeID
        }

        if candidates.count > 1 { return .cancel }
        if !sourceStillExists, let candidate = candidates.first {
            return .select(candidate.id)
        }
        guard intent.bootstrapObservationsRemaining > 1 else { return .cancel }
        var waiting = intent
        waiting.bootstrapObservationsRemaining -= 1
        return .wait(waiting)
    }

    /// Ignore the capture clock and output-preview churn so a two-second
    /// health poll does not rebuild the native sidebar while an agent types.
    static func snapshotContentEqual(
        _ lhs: RemoteBootstrapSnapshot,
        _ rhs: RemoteBootstrapSnapshot
    ) -> Bool {
        lhs.protocolVersion == rhs.protocolVersion
            && lhs.hostProtocol == rhs.hostProtocol
            && lhs.macID == rhs.macID
            && lhs.macName == rhs.macName
            && lhs.folders == rhs.folders
            && lhs.projects == rhs.projects
            && lhs.presets == rhs.presets
            && lhs.remoteServerPort == rhs.remoteServerPort
            && lhs.remoteServerCertificateFingerprint
                == rhs.remoteServerCertificateFingerprint
            && lhs.workspaceSettings == rhs.workspaceSettings
            && lhs.availableApps == rhs.availableApps
            && lhs.installedApps == rhs.installedApps
            && lhs.openers == rhs.openers
            && lhs.appPresentations == rhs.appPresentations
            && lhs.experimentalWorktreesEnabled == rhs.experimentalWorktreesEnabled
            && lhs.proEntitled == rhs.proEntitled
            && lhs.pendingApprovals == rhs.pendingApprovals
            && lhs.hostTintHue == rhs.hostTintHue
            && lhs.hostDeviceKind == rhs.hostDeviceKind
            && lhs.hostDeviceModel == rhs.hostDeviceModel
            && lhs.sessions.count == rhs.sessions.count
            && zip(lhs.sessions, rhs.sessions).allSatisfy(Self.sessionRenderEqual)
    }

    private static func sessionRenderEqual(
        _ lhs: RemoteSessionSummary,
        _ rhs: RemoteSessionSummary
    ) -> Bool {
        lhs.id == rhs.id
            && lhs.projectID == rhs.projectID
            && lhs.activeRuntimeID == rhs.activeRuntimeID
            && lhs.runtimeLaunchPending == rhs.runtimeLaunchPending
            && lhs.providerID == rhs.providerID
            && lhs.title == rhs.title
            && lhs.command == rhs.command
            && lhs.createdAtUnixMs == rhs.createdAtUnixMs
            && minuteBucket(lhs.updatedAtUnixMs) == minuteBucket(rhs.updatedAtUnixMs)
            && lhs.status == rhs.status
            && lhs.activity == rhs.activity
            && lhs.unread == rhs.unread
            && lhs.pinned == rhs.pinned
            && lhs.worktreePath == rhs.worktreePath
            && lhs.worktreeBranch == rhs.worktreeBranch
            && lhs.parentSessionID == rhs.parentSessionID
            && lhs.notifyWhenDone == rhs.notifyWhenDone
            && lhs.terminalBackgroundHex == rhs.terminalBackgroundHex
            && lhs.capabilities == rhs.capabilities
            && lhs.archived == rhs.archived
            && lhs.spinnerColorHex == rhs.spinnerColorHex
            // Alert-only polling changes must reach the desktop activity
            // dropdown even when their timestamps share one minute bucket.
            && lhs.latestAlertBody == rhs.latestAlertBody
            && minuteBucket(lhs.latestAlertAtUnixMs)
                == minuteBucket(rhs.latestAlertAtUnixMs)
    }

    private static func minuteBucket(_ unixMs: Int64?) -> Int64? {
        unixMs.map { $0 / 60_000 }
    }
}

// MARK: - Organization / lifecycle verbs

/// One user-facing failure from a remote organization or lifecycle verb.
/// `outcomeIsUnknown` means the effect may have reached the Host even though
/// its receipt was lost — the UI must surface it and never retry silently.
struct RemoteHostVerbError: Error, LocalizedError, Equatable, Sendable {
    let operation: String
    let message: String
    let outcomeIsUnknown: Bool

    var errorDescription: String? { message }
}

extension RemoteHostRuntime {
    /// Stable Host operation ids (protocol/host-capabilities-v1.json). Menus
    /// gate on these through `supportsHostOperation`; the backend enforces
    /// them again per call.
    enum HostOperation {
        static let write = "session.input.write"
        static let titleSet = "session.title.set"
        static let pinSet = "session.pin.set"
        static let archive = "session.archive"
        static let restore = "session.restore"
        static let stop = "session.stop"
        static let remove = "session.remove"
        static let restart = "session.restart"
        static let resizeDesktop = "session.resize_desktop"
        static let resumeAgent = RemoteControlProtocol.sessionRuntimeResumeCapability
        static let create = "session.create"
        static let orderSet = "session.order.set"
        static let projectOrganizationSet = "project.organization.set"
        static let projectPinSet = "project.pin.set"
        static let presetsSet = RemoteControlProtocol.presetsSetCapability
        static let workspaceSettingsSet = RemoteControlProtocol.workspaceSettingsSetCapability
        static let openersSet = RemoteControlProtocol.openersSetCapability
        static let appsInstall = RemoteControlProtocol.appsInstallCapability
        static let appsOpen = RemoteControlProtocol.appsOpenCapability
        static let archiveList = "session.archive.list"
        static let transcriptMarkdown = "session.transcript.markdown"
        static let pairingInvitation = "pairing.invitation"
        static let artifactUpload = "artifact.upload"
        static let projectSet = "session.project.set"
        static let pushRegister = "push.register"
        static let notifyWhenDoneSet = "session.notify_when_done.set"
        static let approvalAnswer = "approval.answer"
    }

    /// Upload dropped image bytes to the selected Host (`artifact.upload`,
    /// the phone attach flow's operation) and return the HOST-side path the
    /// Controller pastes as an attachable reference.
    func uploadAttachment(
        sessionID: String?,
        contentType: String,
        bytes: Data
    ) async throws -> String {
        var path: String?
        try await performOrganizationVerb(
            capability: HostOperation.artifactUpload,
            operation: "attachment upload"
        ) { backend in
            path = try await backend.uploadAttachment(
                sessionID: sessionID,
                contentType: contentType,
                bytes: bytes
            )
        }
        guard let path else {
            throw RemoteHostVerbError(
                operation: "attachment upload",
                message: "The Host did not return the uploaded file's path.",
                outcomeIsUnknown: true
            )
        }
        return path
    }

    /// Whether the bootstrapped Host advertises one stable operation id.
    /// Never guessed and never probed: absent ledger means unsupported.
    func supportsHostOperation(_ operation: String) -> Bool {
        snapshot?.hostProtocol?.supports(operation) == true
    }

    func renameSession(_ sessionID: String, to title: String) async throws {
        try await performOrganizationVerb(
            capability: HostOperation.titleSet,
            operation: "rename"
        ) { backend in
            _ = try await backend.setSessionTitle(sessionID: sessionID, title: title)
        }
    }

    /// File a Session under another project/group (`session.project.set`) —
    /// the Host's shared project-override move; display only.
    func setSessionProject(_ sessionID: String, projectID: String) async throws {
        try await performOrganizationVerb(
            capability: HostOperation.projectSet,
            operation: "move session"
        ) { backend in
            _ = try await backend.setSessionProject(
                sessionID: sessionID, projectID: projectID
            )
        }
    }

    func setSessionPinned(_ sessionID: String, pinned: Bool) async throws {
        try await performOrganizationVerb(
            capability: HostOperation.pinSet,
            operation: pinned ? "pin" : "unpin"
        ) { backend in
            _ = try await backend.setSessionPinned(sessionID: sessionID, pinned: pinned)
        }
    }

    func setNotifyWhenDone(_ sessionID: String, enabled: Bool) async throws {
        try await performOrganizationVerb(
            capability: HostOperation.notifyWhenDoneSet,
            operation: "update the completion notification"
        ) { backend in
            _ = try await backend.setSessionNotifyWhenDone(
                sessionID: sessionID,
                enabled: enabled
            )
        }
    }

    func answerApproval(_ id: String, approved: Bool) async throws {
        try await performOrganizationVerb(
            capability: HostOperation.approvalAnswer,
            operation: approved ? "allow approval" : "deny approval"
        ) { backend in
            _ = try await backend.answerApproval(id: id, approved: approved)
        }
    }

    func archiveSession(_ sessionID: String) async throws {
        try await performOrganizationVerb(
            capability: HostOperation.archive,
            operation: "archive"
        ) { backend in
            _ = try await backend.archiveSession(sessionID: sessionID)
        }
    }

    func restoreSession(_ sessionID: String) async throws {
        try await performOrganizationVerb(
            capability: HostOperation.restore,
            operation: "restore"
        ) { backend in
            _ = try await backend.restoreSession(sessionID: sessionID)
        }
    }

    func stopSession(_ sessionID: String) async throws {
        try await performOrganizationVerb(
            capability: HostOperation.stop,
            operation: "stop"
        ) { backend in
            _ = try await backend.stopSession(sessionID: sessionID)
        }
    }

    func removeSession(_ sessionID: String) async throws {
        try await performOrganizationVerb(
            capability: HostOperation.remove,
            operation: "remove"
        ) { backend in
            _ = try await backend.removeSession(sessionID: sessionID)
        }
    }

    func restartSession(_ sessionID: String) async throws {
        guard let source = snapshot?.sessions.first(where: { $0.id == sessionID }) else {
            throw RemoteHostVerbError(
                operation: "resume",
                message: "The Session is no longer available on this Host.",
                outcomeIsUnknown: false
            )
        }
        try beginReplacementSelection(
            source,
            knownSessionIDs: Set((snapshot?.sessions ?? []).map(\.id)),
            operation: "resume"
        )
        try await submitReplacementRestart(sessionID)
    }

    private func sendRestartSession(_ sessionID: String) async throws {
        try await performOrganizationVerb(
            capability: HostOperation.restart,
            operation: "restart"
        ) { backend in
            _ = try await backend.restartSession(sessionID: sessionID)
        }
    }

    /// The Host's legacy replacement receipt does not return the new Session
    /// id. Latch correlation before Restore so an immediate bootstrap cannot
    /// race ahead, then keep it through Restart until a unique replacement is
    /// published. This is intentionally one selection intent at a time.
    func restoreAndRestartSession(
        _ source: RemoteSessionSummary,
        knownSessionIDs: Set<String>
    ) async throws {
        try beginReplacementSelection(
            source,
            knownSessionIDs: knownSessionIDs,
            operation: "restore and resume"
        )

        do {
            try await restoreSession(source.id)
        } catch {
            // Restart was never submitted, so no replacement can appear.
            pendingReplacementSelection = nil
            replacementDefaultSelectionSuppressed = false
            throw error
        }

        try await submitReplacementRestart(source.id)
    }

    private func beginReplacementSelection(
        _ source: RemoteSessionSummary,
        knownSessionIDs: Set<String>,
        operation: String
    ) throws {
        guard pendingReplacementSelection == nil else {
            throw RemoteHostVerbError(
                operation: operation,
                message: "Another Session is still being resumed.",
                outcomeIsUnknown: false
            )
        }
        let currentIDs = Set((snapshot?.sessions ?? []).map(\.id))
        pendingCreatedSelectionID = nil
        pendingReplacementSelection = ReplacementSelectionIntent(
            source: source,
            knownSessionIDs: knownSessionIDs.union(currentIDs)
        )
        replacementDefaultSelectionSuppressed = true
    }

    private func submitReplacementRestart(_ sessionID: String) async throws {
        do {
            try await sendRestartSession(sessionID)
        } catch {
            // An outcome-unknown restart may have minted the replacement.
            // Retain the bounded intent in that case; a definitive rejection
            // cannot, and must not steal a later unrelated Session.
            if (error as? RemoteHostVerbError)?.outcomeIsUnknown != true {
                pendingReplacementSelection = nil
                replacementDefaultSelectionSuppressed = false
            }
            throw error
        }
    }

    func resumeAgent(_ sessionID: String) async throws {
        try await performOrganizationVerb(
            capability: HostOperation.resumeAgent,
            operation: "resume agent"
        ) { backend in
            _ = try await backend.resumeAgent(sessionID: sessionID)
        }
    }

    func setSessionOrder(
        projectID: String,
        orderedSessionIDs: [String]
    ) async throws {
        try await performOrganizationVerb(
            capability: HostOperation.orderSet,
            operation: "reorder"
        ) { backend in
            _ = try await backend.setSessionOrder(
                projectID: projectID,
                orderedSessionIDs: orderedSessionIDs
            )
        }
    }

    /// Move a project to `sortOrder` among its same-parent siblings in the
    /// Host's display order — the Controller half of a sidebar project drag
    /// (`project.organization.set`, one-project patch). The Host persists it
    /// through the same path a local drag commits; the refreshed bootstrap
    /// reconciles the projection.
    func setProjectSortOrder(projectID: String, sortOrder: Int) async throws {
        try await setProjectOrganization(
            projectID: projectID,
            operation: "reorder",
            patch: RemoteProjectOrganizationPatch(
                projectID: projectID,
                sortOrder: sortOrder
            )
        )
    }

    func renameProjectGroup(projectID: String, displayName: String) async throws {
        try await setProjectOrganization(
            projectID: projectID,
            operation: "rename group",
            patch: RemoteProjectOrganizationPatch(
                projectID: projectID,
                displayName: displayName
            )
        )
    }

    func setProjectFolderColor(projectID: String, colorID: String?) async throws {
        try await setProjectOrganization(
            projectID: projectID,
            operation: "change folder color",
            patch: RemoteProjectOrganizationPatch(
                projectID: projectID,
                colorID: colorID ?? ""
            )
        )
    }

    func setProjectDateSorted(projectID: String, dateSorted: Bool) async throws {
        try await setProjectOrganization(
            projectID: projectID,
            operation: "change Session sort",
            patch: RemoteProjectOrganizationPatch(
                projectID: projectID,
                dateSorted: dateSorted
            )
        )
    }

    /// Edit the selected workspace's settings (`settings.workspace.set`):
    /// presentation, notification/experimental behavior, transcript options,
    /// cleanup, and MCP access policies. Capability-gated and refreshed like
    /// every organization verb, so the snapshot's `workspaceSettings`
    /// reconciles to what the Host actually holds.
    func setWorkspaceSettings(_ patch: RemoteWorkspaceSettingsPatch) async throws {
        try await performOrganizationVerb(
            capability: HostOperation.workspaceSettingsSet,
            operation: "workspace settings"
        ) { backend in
            _ = try await backend.setWorkspaceSettings(patch: patch)
        }
    }

    func setOpener(selector: String, opener: String) async throws {
        try await performOrganizationVerb(
            capability: HostOperation.openersSet,
            operation: "resource opener"
        ) { backend in
            _ = try await backend.setOpener(selector: selector, opener: opener)
        }
    }

    func installApp(_ appID: String) async throws {
        try await performOrganizationVerb(
            capability: HostOperation.appsInstall,
            operation: "App install"
        ) { backend in
            _ = try await backend.installApp(appID: appID)
        }
    }

    func openApp(
        _ appID: String,
        resourceKind: String,
        mediaType: String?,
        resourceID: String,
        callerSessionID: String
    ) async throws {
        let requestID = UUID().uuidString.lowercased()
        try await performOrganizationVerb(
            capability: HostOperation.appsOpen,
            operation: "App open"
        ) { backend in
            _ = try await backend.openApp(
                callerSessionID: callerSessionID,
                appID: appID,
                resourceKind: resourceKind,
                mediaType: mediaType,
                resourceID: resourceID,
                requestID: requestID
            )
        }
    }

    /// Edit the selected Host's preset list (`settings.presets.set`) — the
    /// Controller half of scoped Settings ▸ Presets
    /// (the scope selector follows the window's active workspace). One-preset
    /// patch, capability-gated, refreshed like every organization verb; a
    /// create's minted id arrives with the refreshed snapshot.
    func setPreset(_ patch: RemotePresetPatch) async throws {
        try await performOrganizationVerb(
            capability: HostOperation.presetsSet,
            operation: "preset edit"
        ) { backend in
            _ = try await backend.setPreset(patch: patch)
        }
    }

    /// Release a PHONE-owned grid the Host published for a Session
    /// (`phoneFitColumns/Rows`): the desktop's "fit to desktop" control. This
    /// is the phone's own `clear` verb sent by the Controller, so it is not
    /// tied to a fit this runtime sent (unlike `clearDesktopFit`), and it is
    /// capability-gated on `session.resize_desktop` like every effect. The
    /// next bootstrap drops the published grid; the caller re-asserts the
    /// desktop size through its ordinary surface resize.
    func clearPhoneFit(sessionID: String) async throws {
        try await performOrganizationVerb(
            capability: HostOperation.resizeDesktop,
            operation: "fit to desktop"
        ) { backend in
            _ = try await backend.clearDesktopFit(sessionID: sessionID)
        }
    }

    /// Pin/unpin a plain group through the shared project-organization route.
    /// The dedicated capability keeps this additive minor-8 field hidden
    /// against older Hosts that only understand the rest of the patch.
    func setProjectPinned(projectID: String, pinned: Bool) async throws {
        try await setProjectOrganization(
            projectID: projectID,
            capability: HostOperation.projectPinSet,
            operation: pinned ? "pin group" : "unpin group",
            patch: RemoteProjectOrganizationPatch(
                projectID: projectID,
                pinned: pinned
            )
        )
    }

    private func setProjectOrganization(
        projectID: String,
        capability: String = HostOperation.projectOrganizationSet,
        operation: String,
        patch: RemoteProjectOrganizationPatch
    ) async throws {
        try await performOrganizationVerb(
            capability: capability,
            operation: operation
        ) { backend in
            _ = try await backend.setProjectOrganization(
                projectID: projectID,
                patch: patch
            )
        }
    }

    /// Create a Session on the Host from an explicit user action (preset chip
    /// or project menu). The created row is selected as soon as a bootstrap
    /// (or the Host's optimistic receipt) reports it.
    @discardableResult
    func createSession(
        projectID: String,
        presetID: String? = nil,
        command: String? = nil,
        // Background creates (the right panel's launcher) must not steal the
        // user's current selection; the new pane appearing IS the feedback.
        selectOnCreate: Bool = true
    ) async throws -> String {
        // A newer explicit selection intent supersedes any unresolved legacy
        // replacement correlation.
        pendingReplacementSelection = nil
        replacementDefaultSelectionSuppressed = false
        var createdReceipt: NativeRemoteCreatedSession?
        try await performOrganizationVerb(
            capability: HostOperation.create,
            operation: "new session"
        ) { backend in
            let created = try await backend.createSession(RemoteCreateSessionRequest(
                projectID: projectID,
                presetID: presetID,
                command: command,
                worktreePath: nil,
                worktreeBranch: nil,
                initialText: nil,
                initialTextSubmitMode: .pasteAndSubmit
            ))
            createdReceipt = created
        }
        guard let createdReceipt else {
            throw RemoteHostVerbError(
                operation: "new session",
                message: "The Host did not return the created Session.",
                outcomeIsUnknown: true
            )
        }
        let createdSessionID = createdReceipt.sessionID
        if selectOnCreate {
            pendingCreatedSelectionID = createdSessionID
        }
        if let optimistic = createdReceipt.session,
           optimistic.id == createdSessionID,
           let snapshot {
            pendingOptimisticCreatedSession = (
                summary: optimistic,
                observationsRemaining: 8
            )
            adopt(snapshot)
        } else if selectOnCreate,
                  snapshot?.sessions.contains(where: { $0.id == createdSessionID }) == true {
            pendingCreatedSelectionID = nil
            if connection?.transport.usesDirectSessionDataPlane == true {
                transitionSelectedSession(to: createdSessionID)
                publishDirectDataPlaneSelectionIntent(createdSessionID)
            } else {
                selectSession(createdSessionID)
            }
        }
        return createdSessionID
    }

    /// One project's archived Sessions — a capability-gated read.
    func archivedSessions(projectID: String) async throws -> [RemoteSessionSummary] {
        let (connection, _) = try requireConnection(
            capability: HostOperation.archiveList,
            operation: "archived sessions"
        )
        do {
            return try await connection.backend.listArchivedSessions(projectID: projectID)
        } catch {
            throw Self.verbError(from: error, operation: "archived sessions", isEffect: false)
        }
    }

    /// One Session's Markdown transcript — a capability-gated read.
    func transcriptMarkdown(sessionID: String, entries: Int?) async throws -> String {
        let (connection, _) = try requireConnection(
            capability: HostOperation.transcriptMarkdown,
            operation: "copy transcript"
        )
        do {
            let transcript = try await connection.backend.transcriptMarkdown(
                sessionID: sessionID,
                entries: entries
            )
            return transcript.markdown
        } catch {
            throw Self.verbError(from: error, operation: "copy transcript", isEffect: false)
        }
    }

    /// Ask the selected Host to mint a one-time pairing grant whose sealed
    /// request will arrive through this Mac's short-lived LAN proxy.
    func createPairingInvitation(proxyEndpoint: URL) async throws -> RemotePairingPayload {
        struct Request: Encodable {
            let action = "create"
            let endpoint: URL
        }
        let body = try JSONEncoder().encode(Request(endpoint: proxyEndpoint))
        let response = try await performPairingInvitation(body)
        do {
            return try JSONDecoder().decode(RemotePairingPayload.self, from: response)
        } catch {
            throw RemoteHostVerbError(
                operation: "pair another controller",
                message: "The Host returned an invalid pairing invitation.",
                outcomeIsUnknown: true
            )
        }
    }

    /// Forward the phone's still-sealed envelope to the selected Host. This
    /// Mac never receives the new phone bearer, Link token, or E2E key in
    /// plaintext; it returns the Host's sealed response byte-for-byte.
    func completePairingInvitation(envelopeJSON: Data) async throws -> Data {
        struct Request: Encodable {
            let action = "complete"
            let envelope: RemotePairingEnvelope
        }
        let envelope: RemotePairingEnvelope
        do {
            envelope = try JSONDecoder().decode(RemotePairingEnvelope.self, from: envelopeJSON)
        } catch {
            throw RemoteHostVerbError(
                operation: "pair another controller",
                message: "The phone sent an invalid pairing envelope.",
                outcomeIsUnknown: false
            )
        }
        return try await performPairingInvitation(
            try JSONEncoder().encode(Request(envelope: envelope))
        )
    }

    // MARK: verb plumbing

    private func requireConnection(
        capability: String,
        operation: String
    ) throws -> (ActiveConnection, UInt64) {
        guard connectionBootstrapped, let connection else {
            throw RemoteHostVerbError(
                operation: operation,
                message: "This Host is not connected.",
                outcomeIsUnknown: false
            )
        }
        guard supportsHostOperation(capability) else {
            throw RemoteHostVerbError(
                operation: operation,
                message: "This Host does not support \(operation) yet. Update Unpeel on the Host.",
                outcomeIsUnknown: false
            )
        }
        return (connection, connection.epoch)
    }

    /// Run one at-most-once organization effect against the current backend
    /// generation. Success and outcome-unknown failures both trigger an
    /// immediate bootstrap refresh; nothing is ever replayed automatically.
    private func performOrganizationVerb(
        capability: String,
        operation: String,
        _ body: (any NativeRemoteBackendProtocol) async throws -> Void
    ) async throws {
        let (connection, _) = try requireConnection(
            capability: capability,
            operation: operation
        )
        // A predecessor connection to the same Host may still be unwinding
        // its effect tail; organization effects respect the same start
        // barrier as terminal effects so they cannot overtake it.
        await connection.effectStartBarrier?.value
        guard isCurrent(connection), connectionBootstrapped else {
            throw RemoteHostVerbError(
                operation: operation,
                message: "The Host connection changed before \(operation) was sent.",
                outcomeIsUnknown: false
            )
        }
        do {
            try await body(connection.backend)
            refreshAfterOrganizationVerb(connection)
        } catch {
            let verbError = Self.verbError(from: error, operation: operation, isEffect: true)
            if verbError.outcomeIsUnknown {
                // The effect may have landed. A fresh bootstrap proves the
                // resulting Host state (and a fresh accepted generation)
                // before anything else is offered.
                refreshAfterOrganizationVerb(connection)
            }
            throw verbError
        }
    }

    private func performPairingInvitation(_ body: Data) async throws -> Data {
        let (connection, _) = try requireConnection(
            capability: HostOperation.pairingInvitation,
            operation: "pair another controller"
        )
        await connection.effectStartBarrier?.value
        guard isCurrent(connection), connectionBootstrapped else {
            throw RemoteHostVerbError(
                operation: "pair another controller",
                message: "The Host connection changed before the invitation was sent.",
                outcomeIsUnknown: false
            )
        }
        do {
            return try await connection.backend.pairingInvitation(body)
        } catch {
            let verbError = Self.verbError(
                from: error,
                operation: "pair another controller",
                isEffect: true
            )
            if verbError.outcomeIsUnknown {
                refreshAfterOrganizationVerb(connection)
            }
            throw verbError
        }
    }

    private func refreshAfterOrganizationVerb(_ connection: ActiveConnection) {
        guard isCurrent(connection) else { return }
        startRefreshLoop(for: connection)
    }

    private static func verbError(
        from error: Error,
        operation: String,
        isEffect: Bool
    ) -> RemoteHostVerbError {
        if let verbError = error as? RemoteHostVerbError { return verbError }
        if let bridgeError = error as? NativeRemoteBackendError {
            return RemoteHostVerbError(
                operation: operation,
                message: bridgeError.message,
                outcomeIsUnknown: isEffect && !bridgeError.effectWasNotApplied
            )
        }
        if error is CancellationError {
            return RemoteHostVerbError(
                operation: operation,
                message: "\(operation) was interrupted; refresh the Host before retrying.",
                outcomeIsUnknown: isEffect
            )
        }
        return RemoteHostVerbError(
            operation: operation,
            message: error.localizedDescription,
            outcomeIsUnknown: isEffect
        )
    }
}
