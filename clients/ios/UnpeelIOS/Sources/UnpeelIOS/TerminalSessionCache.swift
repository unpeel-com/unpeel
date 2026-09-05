import Foundation
import GhosttyTerminal
import UnpeelShared
#if os(iOS)
import UIKit
#endif

/// Pure LRU bookkeeping for the per-session terminal cache. Generic over the
/// payload so the ordering/eviction/prune logic is unit-testable without ever
/// touching renderers or ghostty surfaces (which cannot exist in headless
/// test runs).
struct SessionLRUIndex<Entry> {
    let capacity: Int
    /// Least-recently-used first, most-recently-used last.
    private(set) var order: [String] = []
    private var storage: [String: Entry] = [:]

    init(capacity: Int) {
        self.capacity = max(capacity, 1)
    }

    var count: Int { order.count }
    var keys: [String] { order }

    /// Returns the entry and marks it most-recently-used.
    mutating func lookup(_ id: String) -> Entry? {
        guard let entry = storage[id] else { return nil }
        touch(id)
        return entry
    }

    /// Reads without touching recency (for inspection/iteration).
    func peek(_ id: String) -> Entry? {
        storage[id]
    }

    /// Inserts (or replaces) an entry as most-recently-used. Returns the
    /// entries evicted to stay within capacity — never the one just inserted.
    mutating func insert(_ entry: Entry, for id: String) -> [(id: String, entry: Entry)] {
        storage[id] = entry
        touch(id)
        var evicted: [(id: String, entry: Entry)] = []
        while order.count > capacity {
            let oldest = order.removeFirst()
            if let dropped = storage.removeValue(forKey: oldest) {
                evicted.append((oldest, dropped))
            }
        }
        return evicted
    }

    @discardableResult
    mutating func remove(_ id: String) -> Entry? {
        order.removeAll { $0 == id }
        return storage.removeValue(forKey: id)
    }

    mutating func removeAll() -> [(id: String, entry: Entry)] {
        let removed = order.compactMap { id in storage[id].map { (id, $0) } }
        order.removeAll()
        storage.removeAll()
        return removed
    }

    /// Drops every entry whose id is not in `ids` (session killed on the
    /// Mac), except `keeping` (the on-screen session must not be torn down
    /// under a transiently stale/empty session list).
    mutating func retain(
        only ids: Set<String>,
        keeping: String? = nil
    ) -> [(id: String, entry: Entry)] {
        let doomed = order.filter { $0 != keeping && !ids.contains($0) }
        return doomed.compactMap { id in remove(id).map { (id, $0) } }
    }

    /// Drops everything except `id` (memory pressure: keep only the visible
    /// session's terminal).
    mutating func removeAll(except id: String?) -> [(id: String, entry: Entry)] {
        let doomed = order.filter { $0 != id }
        return doomed.compactMap { id in remove(id).map { (id, $0) } }
    }

    private mutating func touch(_ id: String) {
        order.removeAll { $0 == id }
        order.append(id)
    }
}

/// Token-gated ownership for the one terminal currently on screen. SwiftUI
/// may mount a replacement before dismantling the previous view. The token
/// distinguishes those two lifetimes even when they render the same session
/// (notably a connection-epoch remount), so the old disappear cannot hide the
/// replacement from prune/memory-pressure protection.
struct TerminalVisibilityLeaseTracker {
    private(set) var sessionID: String?
    private var owner: UUID?

    mutating func acquire(sessionID: String, owner: UUID) {
        self.sessionID = sessionID
        self.owner = owner
    }

    @discardableResult
    mutating func release(sessionID: String, owner: UUID) -> Bool {
        guard self.sessionID == sessionID, self.owner == owner else { return false }
        self.sessionID = nil
        self.owner = nil
        return true
    }
}

/// Reference-counts renderer streaming by mounted SwiftUI lifetime. A Set
/// makes repeated appear/scene-phase callbacks idempotent. More importantly,
/// a late disappear from an old mount only releases its own token; it cannot
/// stop a cached renderer already acquired by its replacement.
struct TerminalStreamLeaseTracker {
    private var owners: Set<UUID> = []

    var isEmpty: Bool { owners.isEmpty }

    /// True only for the transition that should start the renderer.
    mutating func acquire(_ owner: UUID) -> Bool {
        let inserted = owners.insert(owner).inserted
        return inserted && owners.count == 1
    }

    /// True only for the transition that should stop the renderer.
    mutating func release(_ owner: UUID) -> Bool {
        guard owners.remove(owner) != nil else { return false }
        return owners.isEmpty
    }

    mutating func removeAll() {
        owners.removeAll()
    }
}

/// One cached session terminal: the renderer (stream state, held output
/// offset, `TerminalViewState`, `InMemoryTerminalSession`) plus — on iOS —
/// the platform `TerminalView`. The UIKit view must be cached too because
/// the ghostty surface (the object that actually holds the terminal screen
/// + scrollback state) is owned by the view's `TerminalSurfaceCoordinator`;
/// with `retainsSurfaceWhenDetached` the surface survives the SwiftUI
/// teardown and re-attaches still painted.
@MainActor
final class TerminalSessionCacheEntry {
    let renderer: RemoteGhosttyRenderer
    private var streamLeases = TerminalStreamLeaseTracker()
    private var isRetired = false

    init(renderer: RemoteGhosttyRenderer) {
        self.renderer = renderer
    }

    /// Adopt or relinquish one mounted surface's stream ownership. Retired
    /// entries ignore stale callbacks from views SwiftUI is still dismantling.
    func setStreaming(_ streaming: Bool, owner: UUID) {
        guard !isRetired else { return }
        if streaming {
            if streamLeases.acquire(owner) {
                renderer.start()
            }
        } else if streamLeases.release(owner) {
            renderer.stop()
        }
    }

    #if os(iOS)
    private(set) var terminalView: TerminalView?

    /// The bridge's makeUIView: reuse the cached platform view (its surface
    /// still holds the last frame) or create the session's first one.
    func makeOrReuseTerminalView() -> TerminalView {
        if let terminalView { return terminalView }
        let view = TerminalView(frame: .zero)
        view.retainsSurfaceWhenDetached = true
        terminalView = view
        return view
    }
    #endif

    /// Tear down the platform view (and with it the ghostty surface) on
    /// eviction. Dropping the flag first makes an attached view free its
    /// surface synchronously on removal; a parked view frees it in the
    /// coordinator's deinit when the last reference goes away here.
    func releaseTerminalView() {
        #if os(iOS)
        terminalView?.retainsSurfaceWhenDetached = false
        terminalView?.removeFromSuperview()
        terminalView = nil
        #endif
    }

    /// Permanently tear down an evicted entry. A later callback from its old
    /// SwiftUI mount must not resurrect its network loop or Metal surface.
    func retire() {
        guard !isRetired else { return }
        isRetired = true
        streamLeases.removeAll()
        renderer.stop()
        releaseTerminalView()
    }
}

/// App-wide cache of live terminal renderers + platform views, keyed by
/// session id and scoped to the connection epoch. Switching back to a
/// recently viewed session reuses its renderer (held `outputOffset`, `
/// initialReplayDone`) and its ghostty surface (last painted frame), so the
/// switch paints instantly and catches up incrementally instead of paying a
/// full grid-fit + tail replay behind a skeleton.
@MainActor
final class TerminalSessionCache {
    static let shared = TerminalSessionCache()

    /// 3 sessions ≈ the "flipping between a couple of agents" working set;
    /// each entry holds a Metal surface, so keep it small.
    static let defaultCapacity = 3

    private var index: SessionLRUIndex<TerminalSessionCacheEntry>
    /// The connection epoch the cached renderers captured their client at.
    /// A different epoch (unpair/re-pair) invalidates everything.
    private var epoch: Int?
    /// The mounted lifetime currently on screen. Token gating matters when an
    /// epoch replacement has the same session id as the view it supersedes.
    private var visibilityLease = TerminalVisibilityLeaseTracker()
    /// Spared from prune/memory flushes so the visible terminal is never torn
    /// down underneath the user.
    var visibleSessionID: String? { visibilityLease.sessionID }
    /// Written once in init (main), read in deinit — safe without isolation
    /// (same pattern as `KeyboardHeightObserver`).
    private nonisolated(unsafe) var memoryWarningObserver: NSObjectProtocol?

    init(capacity: Int = TerminalSessionCache.defaultCapacity) {
        index = SessionLRUIndex(capacity: capacity)
        #if os(iOS)
        memoryWarningObserver = NotificationCenter.default.addObserver(
            forName: UIApplication.didReceiveMemoryWarningNotification,
            object: nil,
            queue: .main
        ) { [weak self] _ in
            MainActor.assumeIsolated {
                self?.handleMemoryWarning()
            }
        }
        #endif
    }

    /// Look up the session's cached terminal, or create one. An epoch change
    /// flushes the whole cache first — every cached renderer captured the
    /// previous client at creation.
    func entry(
        for session: RemoteSessionSummary,
        client: RemoteMacClient,
        epoch: Int
    ) -> TerminalSessionCacheEntry {
        if self.epoch != epoch {
            flushAll()
            self.epoch = epoch
        }
        if let hit = index.lookup(session.id) {
            hit.renderer.updatePresentationProviderID(session.presentationProviderID)
            hit.renderer.updateActiveRuntimeID(session.activeRuntimeID)
            return hit
        }
        let entry = TerminalSessionCacheEntry(
            renderer: RemoteGhosttyRenderer(session: session, client: client)
        )
        for (_, evicted) in index.insert(entry, for: session.id) {
            retire(evicted)
        }
        return entry
    }

    func noteVisible(_ sessionID: String, owner: UUID) {
        visibilityLease.acquire(sessionID: sessionID, owner: owner)
    }

    func noteHidden(_ sessionID: String, owner: UUID) {
        // Appear of the replacement can run before disappear of the old one.
        // Match both session and mount token so a same-session epoch remount
        // gets the same protection as an ordinary session switch.
        visibilityLease.release(sessionID: sessionID, owner: owner)
    }

    /// Evict sessions that no longer exist on the Mac (killed/cleaned up).
    /// The visible session is spared: a transiently empty snapshot (network
    /// blip) must not tear down the terminal the user is looking at.
    func pruneMissingSessions(_ liveIDs: Set<String>) {
        for (_, entry) in index.retain(only: liveIDs, keeping: visibleSessionID) {
            retire(entry)
        }
    }

    func flushAll() {
        for (_, entry) in index.removeAll() {
            retire(entry)
        }
    }

    private func handleMemoryWarning() {
        // Free every parked terminal (renderer + surface); keep only the
        // on-screen one — blanking the visible terminal is worse than a
        // skeleton on the next switch.
        for (_, entry) in index.removeAll(except: visibleSessionID) {
            retire(entry)
        }
    }

    private func retire(_ entry: TerminalSessionCacheEntry) {
        entry.retire()
    }

    deinit {
        if let memoryWarningObserver {
            NotificationCenter.default.removeObserver(memoryWarningObserver)
        }
    }
}
