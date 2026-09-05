//
//  WorkspacePool.swift
//  UnpeelNative
//
//  Workspaces-unification phase 7: the BACKGROUND WORKSPACE POOL. Every known
//  workspace (local registry workspaces over loopback gateways, paired and
//  SSH Hosts over their existing transports) keeps a live, read-only
//  background connection with a cached latest bootstrap snapshot, so:
//
//  - the swipe pager / footer dots / selector can peek REAL sidebar content
//    for a neighbor before the scope switches;
//  - scoping into a pooled workspace renders instantly (the cached snapshot
//    seeds the runtime's published snapshot while its own connection opens);
//  - a BACKGROUND workspace whose session needs input badges its dot and can
//    post one deduplicated macOS notification that rescopes on click.
//
//  Strict separation from the foreground: pool connections are separate
//  `NativeRemoteBackend` instances doing bootstrap READS only — no terminal
//  writes, fits, mark-reads, or organization verbs ever ride them — so the
//  foreground `RemoteHostRuntime`'s generation-bound non-replay semantics are
//  untouched by construction. The workspace the runtime currently serves
//  (foreground scope, or the warm background connection kept across a return
//  to Local) is EXCLUDED from pooling; its snapshots are mirrored in through
//  `noteExternalSnapshot` instead, so exactly one live connection per
//  workspace exists at any time.
//
//  Cost model: local gateway children are cheap and stay always-on. Remote
//  (ssh/paired-direct) connections are keep-alive once reachable, capped at a
//  small concurrency, and exponentially backed off while unreachable. Polling
//  is low-cadence (~25s) with an immediate wake on swipe start, picker open,
//  and app foreground. Local pooling follows WorkspaceFeature in release
//  builds; paired/SSH targets remain behind RemoteHostFeature's development
//  gate.
//

import Foundation
import UnpeelShared

@MainActor
final class WorkspacePool: ObservableObject {
    /// One pooled workspace: the stable key IS the shared workspace-order key
    /// (`WorkspaceListOrder.localKey/pairedKey/sshKey`), so views can join
    /// pool state to `WorkspaceListRowModel.id` directly.
    struct Target {
        let key: String
        let name: String
        let transport: RemoteHostTransport
        /// True for ssh/paired transports: subject to the concurrency cap and
        /// the reachability backoff slot release. Local gateways are cheap
        /// child processes and stay always-on.
        let isRemote: Bool
        let expectedHostID: String?
        /// Change detection without Equatable on the transport: a changed
        /// fingerprint retires and reopens the entry.
        let fingerprint: String
    }

    typealias BackendFactory = @MainActor (
        RemoteHostTransport
    ) throws -> any NativeRemoteBackendProtocol
    typealias AttentionNotifier = @MainActor (
        _ sessionTitle: String,
        _ workspaceName: String,
        _ workspaceKey: String,
        _ sessionID: String
    ) -> Void

    /// Latest accepted bootstrap per workspace key. Retained across a lend
    /// (scope entry) so the peek/seed content survives the handoff; dropped
    /// only when the workspace itself is forgotten. A poll whose snapshot is
    /// EQUIVALENT to the cached one (everything but the capture timestamp)
    /// is never re-published — an unchanged workspace must not wake SwiftUI
    /// every ~25s poll (`acceptSnapshot`).
    @Published private(set) var snapshots: [String: RemoteBootstrapSnapshot] = [:]
    /// Compact activity projections built only when an accepted bootstrap
    /// changes. Menu redraws read these instead of walking every project in
    /// every workspace again.
    private var activitySlices: [String: WorkspaceActivityMenuSlice] = [:]
    /// Workspace keys whose snapshot currently carries a blocked (needs
    /// input) live session. Presentation decides scoped-row exclusion; the
    /// notification path excludes the foreground workspace itself.
    @Published private(set) var attentionKeys: Set<String> = []
    /// Last successful contact per workspace. Deliberately NOT `@Published`:
    /// it advances on EVERY successful poll (including equivalent-snapshot
    /// polls), nothing renders from it, and publishing it would defeat the
    /// unchanged-snapshot skip above.
    private(set) var lastSeenAt: [String: Date] = [:]

    private let pollIntervalNanoseconds: UInt64
    private let backoffBaseNanoseconds: UInt64
    private let backoffCapNanoseconds: UInt64
    private let maintenanceIntervalNanoseconds: UInt64
    private let immediateRefreshThrottleSeconds: TimeInterval
    private let maxLiveRemoteConnections: Int
    private let backendFactory: BackendFactory
    private let notifyAttention: AttentionNotifier

    private var targetsProvider: (@MainActor () -> [Target])?
    private var excludedKeys: (@MainActor () -> Set<String>) = { [] }
    private var foregroundKey: (@MainActor () -> String?) = { nil }

    private var entries: [String: Entry] = [:]
    private var latches: [String: AttentionLatch] = [:]
    /// Fingerprints whose Host reported a different identity than the saved
    /// one. Fail closed (stop polling) until the target itself changes — a
    /// re-pair or edited SSH record mints a new fingerprint.
    private var failedIdentityFingerprints: Set<String> = []
    private var remoteSlotsInUse = 0
    private var slotWaiters: [SlotWaiter] = []
    private var maintenanceTask: Task<Void, Never>?
    private var generationCounter: UInt64 = 0
    private var started = false
    private var lastImmediateRefreshAt = Date.distantPast

    /// A foreground reorder is visible before the Host's confirming
    /// bootstrap arrives. Keep that exact order in the pooled snapshot too,
    /// or swiping away during the refresh window shows the old "ghost"
    /// sidebar. Incoming stale bootstraps are projected through these holds;
    /// confirmation (or the bounded timeout) releases Host truth again.
    private struct ProjectOrderHold {
        let parentID: String?
        let ids: [String]
        let heldAt: Date
    }
    private struct SessionOrderHold {
        let ids: [String]
        let heldAt: Date
    }
    private var projectOrderHolds: [String: ProjectOrderHold] = [:]
    private var sessionOrderHolds: [String: [String: SessionOrderHold]] = [:]
    private static let organizationHoldSeconds: TimeInterval = 15

    /// Per-(workspace, session) notification latch: a session notifies once
    /// per blocked EDGE. Leaving blocked clears its latch; the first snapshot
    /// of a workspace seeds silently so app launch never replays stale
    /// attention as a banner storm.
    private struct AttentionLatch {
        var seeded = false
        var notifiedSessionIDs: Set<String> = []
    }

    private struct SlotWaiter {
        let key: String
        let generation: UInt64
        let continuation: CheckedContinuation<Void, Never>
    }

    /// Close-once wrapper: retirement can race an in-flight (possibly
    /// blocking-FFI) bootstrap, so close must be independently callable from
    /// both the retire path and the loop's own exit — exactly once. Same
    /// pattern as the runtime's RouteProbeCandidate.
    private final class PooledBackend: @unchecked Sendable {
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

    private final class Entry {
        let target: Target
        let generation: UInt64
        var task: Task<Void, Never>?
        var pooledBackend: PooledBackend?
        var consecutiveFailures = 0
        var holdsRemoteSlot = false
        var wake: CheckedContinuation<Void, Never>?

        init(target: Target, generation: UInt64) {
            self.target = target
            self.generation = generation
        }
    }

    init(
        pollIntervalNanoseconds: UInt64 = 25_000_000_000,
        backoffBaseNanoseconds: UInt64 = 5_000_000_000,
        backoffCapNanoseconds: UInt64 = 300_000_000_000,
        maintenanceIntervalNanoseconds: UInt64 = 30_000_000_000,
        immediateRefreshThrottleSeconds: TimeInterval = 2,
        maxLiveRemoteConnections: Int = 4,
        backendFactory: @escaping BackendFactory = { transport in
            try RemoteHostBackendFactory.make(transport)
        },
        notifyAttention: @escaping AttentionNotifier = { title, workspace, key, sessionID in
            DesktopNotifier.shared.notifyWorkspaceAttention(
                sessionTitle: title,
                workspaceName: workspace,
                workspaceKey: key,
                sessionID: sessionID
            )
        }
    ) {
        self.pollIntervalNanoseconds = pollIntervalNanoseconds
        self.backoffBaseNanoseconds = backoffBaseNanoseconds
        self.backoffCapNanoseconds = backoffCapNanoseconds
        self.maintenanceIntervalNanoseconds = maintenanceIntervalNanoseconds
        self.immediateRefreshThrottleSeconds = immediateRefreshThrottleSeconds
        self.maxLiveRemoteConnections = max(1, maxLiveRemoteConnections)
        self.backendFactory = backendFactory
        self.notifyAttention = notifyAttention
    }

    // MARK: - Lifecycle

    /// Wire the pool to its owner. `targetsProvider` returns EVERY known
    /// non-current workspace; `excludedKeys` are the workspaces the runtime
    /// currently serves (foreground or warm — the pool must never open a
    /// second live connection to them); `foregroundKey` suppresses attention
    /// notifications for the scoped workspace only (its own per-session
    /// notifications already cover it).
    func start(
        targetsProvider: @escaping @MainActor () -> [Target],
        excludedKeys: @escaping @MainActor () -> Set<String>,
        foregroundKey: @escaping @MainActor () -> String?
    ) {
        guard !started else { return }
        started = true
        self.targetsProvider = targetsProvider
        self.excludedKeys = excludedKeys
        self.foregroundKey = foregroundKey
        maintenanceTask = Task { [weak self] in
            while !Task.isCancelled {
                guard let self else { return }
                try? await Task.sleep(
                    nanoseconds: self.maintenanceIntervalNanoseconds
                )
                if Task.isCancelled { return }
                self.refreshTargets()
            }
        }
        // First reconcile promptly but off the caller's stack. Startup grace
        // is the CALLER's job now: the store schedules `start` ~2s after the
        // first window paint (`startWorkspacePoolAfterFirstPaint`), which
        // also means throwaway stores (preset self-test — no window) never
        // spin the pool up at all.
        Task { [weak self] in
            guard !Task.isCancelled else { return }
            self?.refreshTargets()
        }
    }

    func stop() {
        guard started else { return }
        started = false
        maintenanceTask?.cancel()
        maintenanceTask = nil
        for key in Array(entries.keys) {
            retireEntry(forKey: key)
        }
    }

    // MARK: - Reconciliation

    /// Re-read the known-workspace list and converge entries on it: vanished
    /// workspaces drop their caches, runtime-served keys are retired (cache
    /// kept — the runtime mirror feeds them), and new/changed targets start
    /// polling.
    func refreshTargets() {
        guard started, let targetsProvider else { return }
        let targets = targetsProvider()
        let excluded = excludedKeys()
        var targetsByKey: [String: Target] = [:]
        for target in targets {
            targetsByKey[target.key] = target
        }

        for (key, entry) in entries {
            let target = targetsByKey[key]
            if target == nil
                || excluded.contains(key)
                || target?.fingerprint != entry.target.fingerprint {
                retireEntry(forKey: key)
            }
        }
        // A workspace that is no longer known at all loses its cache; a
        // merely excluded (runtime-served) one keeps it for peek/seed.
        for key in snapshots.keys where targetsByKey[key] == nil && !excluded.contains(key) {
            snapshots.removeValue(forKey: key)
            activitySlices.removeValue(forKey: key)
            lastSeenAt.removeValue(forKey: key)
            latches.removeValue(forKey: key)
            attentionKeys.remove(key)
            projectOrderHolds.removeValue(forKey: key)
            sessionOrderHolds.removeValue(forKey: key)
        }
        for target in targets {
            guard entries[target.key] == nil,
                  !excluded.contains(target.key),
                  !failedIdentityFingerprints.contains(target.fingerprint)
            else { continue }
            startEntry(target)
        }
    }

    /// Wake every sleeping poll loop (and re-reconcile) now. Throttled so
    /// hover-adjacent call sites cannot hammer remote hosts.
    func requestImmediateRefresh() {
        guard started else { return }
        let now = Date()
        guard now.timeIntervalSince(lastImmediateRefreshAt)
            > immediateRefreshThrottleSeconds else { return }
        lastImmediateRefreshAt = now
        refreshTargets()
        for entry in entries.values {
            resumeWake(entry)
        }
    }

    // MARK: - Scope handoff

    /// Scope entry adoption: return the cached snapshot for instant rendering
    /// and retire the pool's read-only connection so the foreground runtime
    /// becomes the only live connection to this workspace. The entry stays
    /// retired until a later reconcile readmits the key — the owner's target
    /// exclusion keeps it out while the runtime serves it.
    func lendConnection(forKey key: String) -> RemoteBootstrapSnapshot? {
        retireEntry(forKey: key)
        return snapshots[key]
    }

    /// Mirror a snapshot owned by another connection (the runtime's live
    /// foreground/warm connection) into the cache, so peek content and
    /// attention detection stay uniform across pooled and lent workspaces.
    func noteExternalSnapshot(
        _ snapshot: RemoteBootstrapSnapshot,
        forKey key: String,
        name: String
    ) {
        guard started else { return }
        acceptSnapshot(snapshot, key: key, name: name)
    }

    /// Mirror a just-committed foreground project reorder into its cached
    /// carousel page immediately, and keep it over stale bootstrap polls
    /// until the Host confirms the same relative order.
    func holdProjectOrder(
        forKey key: String,
        parentID: String?,
        orderedIDs: [String]
    ) {
        projectOrderHolds[key] = ProjectOrderHold(
            parentID: parentID,
            ids: orderedIDs,
            heldAt: Date()
        )
        if let snapshot = snapshots[key] {
            let updated = snapshot.applyingProjectOrder(
                parentID: parentID,
                orderedIDs: orderedIDs
            )
            activitySlices[key] = WorkspaceActivityMenuSlice(snapshot: updated)
            snapshots[key] = updated
        }
    }

    func clearProjectOrderHold(forKey key: String) {
        projectOrderHolds.removeValue(forKey: key)
    }

    /// Session counterpart to `holdProjectOrder`; keyed per project because
    /// independent lists may be reordered before either confirming poll.
    func holdSessionOrder(
        forKey key: String,
        projectID: String,
        orderedIDs: [String]
    ) {
        sessionOrderHolds[key, default: [:]][projectID] = SessionOrderHold(
            ids: orderedIDs,
            heldAt: Date()
        )
        if let snapshot = snapshots[key] {
            let updated = snapshot.applyingSessionOrder(
                projectID: projectID,
                orderedIDs: orderedIDs
            )
            activitySlices[key] = WorkspaceActivityMenuSlice(snapshot: updated)
            snapshots[key] = updated
        }
    }

    func clearSessionOrderHold(forKey key: String, projectID: String) {
        sessionOrderHolds[key]?.removeValue(forKey: projectID)
        if sessionOrderHolds[key]?.isEmpty == true {
            sessionOrderHolds.removeValue(forKey: key)
        }
    }

    /// Drop a cached peek/seed snapshot after an out-of-band home write
    /// (moving a project into this workspace) so the next scope-in does not
    /// render the pre-move tree.
    func dropSnapshot(forKey key: String) {
        snapshots.removeValue(forKey: key)
        activitySlices.removeValue(forKey: key)
        lastSeenAt.removeValue(forKey: key)
    }

    // MARK: - Reads

    func snapshot(forKey key: String) -> RemoteBootstrapSnapshot? {
        snapshots[key]
    }

    func activitySlice(forKey key: String) -> WorkspaceActivityMenuSlice? {
        activitySlices[key]
    }

    func hasAttention(forKey key: String) -> Bool {
        attentionKeys.contains(key)
    }

    // MARK: - Entry loops

    private func startEntry(_ target: Target) {
        generationCounter &+= 1
        let entry = Entry(target: target, generation: generationCounter)
        entries[target.key] = entry
        entry.task = Task { [weak self] in
            await self?.runEntryLoop(entry)
        }
    }

    private func retireEntry(forKey key: String) {
        guard let entry = entries.removeValue(forKey: key) else { return }
        entry.task?.cancel()
        resumeWake(entry)
        if let index = slotWaiters.firstIndex(where: {
            $0.key == key && $0.generation == entry.generation
        }) {
            slotWaiters.remove(at: index).continuation.resume()
        }
        releaseRemoteSlot(entry)
        if let pooled = entry.pooledBackend {
            entry.pooledBackend = nil
            Task { await pooled.close() }
        }
    }

    private func isCurrent(_ entry: Entry) -> Bool {
        entries[entry.target.key] === entry
    }

    private func runEntryLoop(_ entry: Entry) async {
        while isCurrent(entry), !Task.isCancelled {
            if entry.target.isRemote, !entry.holdsRemoteSlot {
                await acquireRemoteSlot(entry)
                guard isCurrent(entry), !Task.isCancelled else { return }
            }

            if entry.pooledBackend == nil {
                do {
                    entry.pooledBackend = PooledBackend(
                        backend: try backendFactory(entry.target.transport)
                    )
                } catch {
                    if !(await backOff(entry)) { return }
                    continue
                }
            }
            guard let pooled = entry.pooledBackend else { continue }

            do {
                let snapshot = try await pooled.backend.bootstrap()
                guard isCurrent(entry), !Task.isCancelled else {
                    await pooled.close()
                    return
                }
                if let expected = entry.target.expectedHostID,
                   snapshot.macID != expected {
                    // Identity fail-closes exactly like the runtime: never
                    // keep reading a Host that no longer matches the saved
                    // identity. The fingerprint latch stops re-opens until
                    // the record itself changes.
                    failedIdentityFingerprints.insert(entry.target.fingerprint)
                    retireEntry(forKey: entry.target.key)
                    return
                }
                entry.consecutiveFailures = 0
                acceptSnapshot(
                    snapshot,
                    key: entry.target.key,
                    name: entry.target.name
                )
                if !(await entrySleep(entry, nanoseconds: pollIntervalNanoseconds)) {
                    return
                }
            } catch {
                guard isCurrent(entry), !Task.isCancelled else { return }
                entry.consecutiveFailures += 1
                entry.pooledBackend = nil
                await pooled.close()
                guard isCurrent(entry), !Task.isCancelled else { return }
                if !(await backOff(entry)) { return }
            }
        }
    }

    /// Unreachable host: close cost is already paid; release the remote slot
    /// for the backoff wait so one dead host never starves a live one, then
    /// sleep the exponential delay.
    private func backOff(_ entry: Entry) async -> Bool {
        releaseRemoteSlot(entry)
        return await entrySleep(
            entry,
            nanoseconds: backoffDelayNanoseconds(entry.consecutiveFailures)
        )
    }

    private func backoffDelayNanoseconds(_ consecutiveFailures: Int) -> UInt64 {
        let exponent = max(0, min(consecutiveFailures - 1, 16))
        let multiplier = UInt64(1) << UInt64(exponent)
        let uncapped = backoffBaseNanoseconds.multipliedReportingOverflow(
            by: multiplier
        )
        let delay = uncapped.overflow ? backoffCapNanoseconds : uncapped.partialValue
        return min(delay, backoffCapNanoseconds)
    }

    /// Interruptible sleep, same shape as the runtime's poll sleep: an
    /// immediate-refresh wake resumes it early. Returns false when the entry
    /// was retired while sleeping.
    private func entrySleep(_ entry: Entry, nanoseconds: UInt64) async -> Bool {
        guard isCurrent(entry), !Task.isCancelled else { return false }
        let sleepTask = Task {
            try? await Task.sleep(nanoseconds: nanoseconds)
        }
        await withCheckedContinuation { (continuation: CheckedContinuation<Void, Never>) in
            entry.wake = continuation
            Task { [weak self] in
                await sleepTask.value
                self?.resumeWake(entry)
            }
        }
        sleepTask.cancel()
        return isCurrent(entry) && !Task.isCancelled
    }

    private func resumeWake(_ entry: Entry) {
        guard let continuation = entry.wake else { return }
        entry.wake = nil
        continuation.resume()
    }

    // MARK: - Remote connection slots

    private func acquireRemoteSlot(_ entry: Entry) async {
        if remoteSlotsInUse < maxLiveRemoteConnections {
            remoteSlotsInUse += 1
            entry.holdsRemoteSlot = true
            return
        }
        await withCheckedContinuation { continuation in
            slotWaiters.append(SlotWaiter(
                key: entry.target.key,
                generation: entry.generation,
                continuation: continuation
            ))
        }
    }

    private func releaseRemoteSlot(_ entry: Entry) {
        guard entry.holdsRemoteSlot else { return }
        entry.holdsRemoteSlot = false
        remoteSlotsInUse -= 1
        grantNextSlot()
    }

    private func grantNextSlot() {
        while remoteSlotsInUse < maxLiveRemoteConnections, !slotWaiters.isEmpty {
            let waiter = slotWaiters.removeFirst()
            if let entry = entries[waiter.key], entry.generation == waiter.generation {
                remoteSlotsInUse += 1
                entry.holdsRemoteSlot = true
            }
            // A stale waiter resumes without a slot; its loop's currency
            // guard exits immediately.
            waiter.continuation.resume()
        }
    }

    // MARK: - Snapshot acceptance + attention

    private func acceptSnapshot(
        _ snapshot: RemoteBootstrapSnapshot,
        key: String,
        name: String
    ) {
        lastSeenAt[key] = Date()
        let snapshot = applyingOrganizationHolds(to: snapshot, key: key)
        // Unchanged workspace → no publish, no SwiftUI wake-up: only the
        // capture timestamp advances on an idle host's poll. The cached
        // snapshot (and its `capturedAtUnixMs`-keyed derived caches, e.g.
        // the pooled sidebar page builder) stays as-is; the latch/attention
        // bookkeeping below would be a no-op for identical content anyway.
        if let cached = snapshots[key], cached.isEquivalent(to: snapshot) {
            return
        }
        // Set the derived cache first: @Published emits willChange before the
        // snapshot assignment, and observers must see an aligned pair on the
        // following runloop turn.
        activitySlices[key] = WorkspaceActivityMenuSlice(snapshot: snapshot)
        snapshots[key] = snapshot

        let blocked = Set(
            snapshot.sessions
                .filter { $0.status == .running && $0.activity == .blocked && !$0.archived }
                .map(\.id)
        )
        var latch = latches[key] ?? AttentionLatch()
        let isForeground = foregroundKey() == key
        if !latch.seeded || isForeground {
            // First contact seeds silently (stale attention at launch is a
            // badge, not a banner). The foreground workspace latches without
            // notifying: the user is looking at it and its own per-session
            // notification pipeline already covers it.
            latch.notifiedSessionIDs = blocked
        } else {
            let newlyBlocked = blocked.subtracting(latch.notifiedSessionIDs)
            for sessionID in newlyBlocked.sorted() {
                guard let session = snapshot.sessions.first(where: { $0.id == sessionID })
                else { continue }
                let title = session.title.isEmpty ? session.command : session.title
                notifyAttention(title, name, key, sessionID)
            }
            // Sessions that left blocked re-arm; still-blocked stay latched,
            // so a persistent prompt notifies exactly once per edge.
            latch.notifiedSessionIDs = blocked
        }
        latch.seeded = true
        latches[key] = latch

        let hasAttention = !blocked.isEmpty
        if hasAttention, !attentionKeys.contains(key) {
            attentionKeys.insert(key)
        } else if !hasAttention, attentionKeys.contains(key) {
            attentionKeys.remove(key)
        }
    }

    private func applyingOrganizationHolds(
        to incoming: RemoteBootstrapSnapshot,
        key: String
    ) -> RemoteBootstrapSnapshot {
        var snapshot = incoming
        let now = Date()

        if let hold = projectOrderHolds[key] {
            let memberIDs = Set(incoming.projects.compactMap { project in
                project.parentProjectID == hold.parentID ? project.id : nil
            })
            let expected = hold.ids.filter { memberIDs.contains($0) }
            let expectedSet = Set(expected)
            let natural = incoming.projects.map(\.id).filter { expectedSet.contains($0) }
            if natural == expected
                || now.timeIntervalSince(hold.heldAt) > Self.organizationHoldSeconds {
                projectOrderHolds.removeValue(forKey: key)
            } else {
                snapshot = snapshot.applyingProjectOrder(
                    parentID: hold.parentID,
                    orderedIDs: hold.ids
                )
            }
        }

        if let holds = sessionOrderHolds[key] {
            for (projectID, hold) in holds {
                let memberIDs = Set(incoming.sessions.compactMap { session in
                    session.projectID == projectID ? session.id : nil
                })
                let expected = hold.ids.filter { memberIDs.contains($0) }
                let expectedSet = Set(expected)
                let natural = incoming.sessions.map(\.id).filter { expectedSet.contains($0) }
                if natural == expected
                    || now.timeIntervalSince(hold.heldAt) > Self.organizationHoldSeconds {
                    clearSessionOrderHold(forKey: key, projectID: projectID)
                } else {
                    snapshot = snapshot.applyingSessionOrder(
                        projectID: projectID,
                        orderedIDs: hold.ids
                    )
                }
            }
        }
        return snapshot
    }
}

extension RemoteBootstrapSnapshot {
    /// Published-cache equivalence: full `Equatable` equality with the
    /// capture timestamp normalized away (it advances on every poll even
    /// when nothing else changed). Built on `==` over a re-stamped copy —
    /// never a field subset — so a future snapshot field can't silently fall
    /// out of the comparison and pin stale content in the cache.
    func isEquivalent(to other: RemoteBootstrapSnapshot) -> Bool {
        withCapturedAt(other.capturedAtUnixMs) == other
    }

    func withCapturedAt(_ capturedAtUnixMs: Int64) -> RemoteBootstrapSnapshot {
        RemoteBootstrapSnapshot(
            protocolVersion: protocolVersion,
            hostProtocol: hostProtocol,
            macID: macID,
            macName: macName,
            folders: folders,
            projects: projects,
            presets: presets,
            workspaceSettings: workspaceSettings,
            sessions: sessions,
            capturedAtUnixMs: capturedAtUnixMs,
            paneGroups: paneGroups,
            remoteServerPort: remoteServerPort,
            remoteServerCertificateFingerprint: remoteServerCertificateFingerprint,
            directEndpoint: directEndpoint,
            experimentalWorktreesEnabled: experimentalWorktreesEnabled,
            proEntitled: proEntitled,
            pendingApprovals: pendingApprovals,
            hostTintHue: hostTintHue,
            hostDeviceKind: hostDeviceKind,
            hostDeviceModel: hostDeviceModel,
            hostIsolationTier: hostIsolationTier,
            hostEnvironment: hostEnvironment,
            hostWorkspaces: hostWorkspaces
        )
    }

    /// Replace only the relative positions named by `orderedIDs`; unknown or
    /// newly-created siblings keep their slots. Array order is the bootstrap
    /// display-order contract consumed by both live and pooled projections.
    func applyingProjectOrder(
        parentID: String?,
        orderedIDs: [String]
    ) -> RemoteBootstrapSnapshot {
        let siblingIDs = Set(projects.compactMap { project in
            project.parentProjectID == parentID ? project.id : nil
        })
        let preferred = orderedIDs.filter { siblingIDs.contains($0) }
        let reordered = Self.applyingRelativeOrder(
            preferred, to: projects, id: \.id
        )
        return replacing(projects: reordered, sessions: sessions)
    }

    /// Optimistically reorder one project's Session summaries and its mixed
    /// `sessionOrder` field (when present), preserving child-folder slots.
    func applyingSessionOrder(
        projectID: String,
        orderedIDs: [String]
    ) -> RemoteBootstrapSnapshot {
        let memberIDs = Set(sessions.compactMap { session in
            session.projectID == projectID ? session.id : nil
        })
        let preferred = orderedIDs.filter { memberIDs.contains($0) }
        let reorderedSessions = Self.applyingRelativeOrder(
            preferred, to: sessions, id: \.id
        )
        let reorderedProjects = projects.map { project in
            guard project.id == projectID, let mixed = project.sessionOrder else {
                return project
            }
            return project.replacingSessionOrder(
                Self.applyingRelativeOrder(preferred, to: mixed, id: { $0 })
            )
        }
        return replacing(projects: reorderedProjects, sessions: reorderedSessions)
    }

    private static func applyingRelativeOrder<Value>(
        _ preferredIDs: [String],
        to values: [Value],
        id: (Value) -> String
    ) -> [Value] {
        let byID = Dictionary(values.map { (id($0), $0) }, uniquingKeysWith: { _, last in last })
        let preferred = preferredIDs.compactMap { byID[$0] }
        let replacingIDs = Set(preferred.map(id))
        var iterator = preferred.makeIterator()
        return values.map { value in
            replacingIDs.contains(id(value)) ? (iterator.next() ?? value) : value
        }
    }

    private func replacing(
        projects: [RemoteProjectSummary],
        sessions: [RemoteSessionSummary]
    ) -> RemoteBootstrapSnapshot {
        RemoteBootstrapSnapshot(
            protocolVersion: protocolVersion,
            hostProtocol: hostProtocol,
            macID: macID,
            macName: macName,
            folders: folders,
            projects: projects,
            presets: presets,
            workspaceSettings: workspaceSettings,
            sessions: sessions,
            capturedAtUnixMs: capturedAtUnixMs,
            paneGroups: paneGroups,
            remoteServerPort: remoteServerPort,
            remoteServerCertificateFingerprint: remoteServerCertificateFingerprint,
            directEndpoint: directEndpoint,
            experimentalWorktreesEnabled: experimentalWorktreesEnabled,
            proEntitled: proEntitled,
            pendingApprovals: pendingApprovals,
            hostTintHue: hostTintHue,
            hostDeviceKind: hostDeviceKind,
            hostDeviceModel: hostDeviceModel,
            hostIsolationTier: hostIsolationTier,
            hostEnvironment: hostEnvironment,
            hostWorkspaces: hostWorkspaces
        )
    }
}

/// Shared transport → backend construction (the same mapping the foreground
/// runtime's default factory uses), so the pool's read-only connections ride
/// the exact transports of their workspaces.
enum RemoteHostBackendFactory {
    @MainActor
    static func make(
        _ transport: RemoteHostTransport
    ) throws -> any NativeRemoteBackendProtocol {
        switch transport {
        case let .ssh(target, expectedHostID, mode, secret):
            return try NativeRemoteBackend(
                sshTarget: target,
                expectedHostID: expectedHostID,
                mode: mode,
                secret: secret
            )
        case let .localGateway(unpeelHome, _, expectedHostID):
            return try NativeRemoteBackend(
                localGatewayHome: unpeelHome,
                expectedHostID: expectedHostID
            )
        case let .localService(unpeelHome, _, expectedHostID):
            return try NativeRemoteBackend(
                localGatewayHome: unpeelHome,
                expectedHostID: expectedHostID,
                requireHostService: true
            )
        case let .direct(endpoint, authToken, expectedHostID):
            return try NativeRemoteBackend(
                directEndpoint: endpoint,
                authToken: authToken,
                expectedHostID: expectedHostID
            )
        case let .link(credentials, controllerDeviceID, authToken, expectedHostID):
            return try NativeRemoteBackend(
                relayCredentials: credentials,
                controllerDeviceID: controllerDeviceID,
                authToken: authToken,
                expectedHostID: expectedHostID
            )
        }
    }
}
