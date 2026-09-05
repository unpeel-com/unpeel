import Combine
import Foundation

/// Window-owned pane presentation and persistence.
///
/// Layouts are Controller state: they are keyed by the window and selected
/// Host scope in the Controller's own home. They never belong to a workspace
/// home, a Host, or a Session record.
@MainActor
final class PaneLayoutController: ObservableObject {
    nonisolated static let storageFileName = "pane-layouts.json"

    struct FocusRequest: Equatable, Sendable {
        let groupID: String
        let paneID: String
        let sessionID: String
        let nonce: UUID
    }

    struct PendingReveal: Equatable, Sendable {
        let groupID: String
        let paneID: String
    }

    /// The pane currently receiving keyboard focus in this window. This is
    /// presentation state, not the sidebar/navigation selection, and is never
    /// persisted. Launcher panes intentionally carry no Session id.
    struct ActivePane: Equatable, Sendable {
        let groupID: String
        let paneID: String
        let sessionID: String?
    }

    /// A temporarily maximized pane. Process-local presentation state: never
    /// persisted, never shared with the other frontend, cleared on scope
    /// switch and whenever the pane leaves the layout.
    struct ZoomedPane: Equatable, Sendable {
        let groupID: String
        let paneID: String
    }

    @Published private(set) var state: PaneLayoutState
    @Published private(set) var dropTarget: PaneDropTarget?
    @Published private(set) var focusRequest: FocusRequest?
    @Published private(set) var pendingReveal: PendingReveal?
    @Published private(set) var activePane: ActivePane?
    @Published private(set) var zoomedPane: ZoomedPane?
    @Published private(set) var scopeID: String

    let windowID: String
    let fileURL: URL
    private var inMemoryStates: [String: PaneLayoutState] = [:]
    private var lastFileRevision: TimeInterval?

    /// `LaunchConfig.unpeelDir` is this Controller process's home. Switching
    /// scope never substitutes a selected workspace or remote Host home.
    convenience init(
        controllerHome: URL = LaunchConfig.unpeelDir,
        windowID: String = "main",
        scopeID: String = "local"
    ) {
        self.init(
            fileURL: controllerHome.appendingPathComponent(Self.storageFileName),
            windowID: windowID,
            scopeID: scopeID
        )
    }

    init(
        fileURL: URL,
        windowID: String = "main",
        scopeID: String = "local"
    ) {
        self.fileURL = fileURL
        self.windowID = windowID
        self.scopeID = scopeID
        let restored = Self.loadState(
            from: fileURL,
            windowID: windowID,
            scopeID: scopeID
        )
        state = restored
        inMemoryStates[scopeID] = restored
        lastFileRevision = Self.fileRevision(fileURL)
    }

    /// Runs one all-or-nothing in-memory state transaction. A throwing body
    /// leaves both the published state and the durable projection unchanged.
    /// Launchers may remain in the live state, but `DurablePaneLayout` omits
    /// them from the atomic Controller-file write.
    @discardableResult
    func mutate<T>(
        _ body: (inout PaneLayoutState) throws -> T
    ) rethrows -> T {
        var candidate = state
        let result = try body(&candidate)
        guard candidate != state else { return result }

        persist(candidate)
        state = candidate
        inMemoryStates[scopeID] = candidate
        lastFileRevision = Self.fileRevision(fileURL)
        return result
    }

    /// Adopt durable layouts another frontend wrote. Keeps a live launcher
    /// when the on-disk projection still matches this process's last persist.
    func reloadFromDisk() {
        switch Self.loadStorageResult(from: fileURL) {
        case .missing, .unreadable:
            return
        case .loaded:
            break
        }
        let revision = Self.fileRevision(fileURL)
        if let revision, revision == lastFileRevision { return }
        let disk = Self.loadState(
            from: fileURL,
            windowID: windowID,
            scopeID: scopeID
        )
        let liveDurable = DurablePaneLayout(state: state)
        let diskDurable = DurablePaneLayout(state: disk)
        guard liveDurable != diskDurable else {
            lastFileRevision = revision
            return
        }
        state = disk
        inMemoryStates[scopeID] = disk
        lastFileRevision = revision
    }

    /// Changes the Host scope shown by this window. Presentation requests are
    /// scoped to one mounted view and must never follow a scope switch.
    func switchScope(to scopeID: String) {
        guard self.scopeID != scopeID else { return }

        inMemoryStates[self.scopeID] = state
        self.scopeID = scopeID
        clearTransientPresentationState()
        let restored = inMemoryStates[scopeID] ?? Self.loadState(
            from: fileURL,
            windowID: windowID,
            scopeID: scopeID
        )
        inMemoryStates[scopeID] = restored
        if state != restored {
            state = restored
        }
    }

    /// Read one scope's layout without changing the window's visible scope or
    /// clearing any transient presentation state. The sidebar workspace
    /// carousel uses this to render the incoming page with the pane groups it
    /// will restore on commit. Prefer the process-local copy: it is the same
    /// value `switchScope` will adopt, including a layout whose last durable
    /// write failed closed to preserve an unreadable/future storage file.
    func state(forScopeID scopeID: String) -> PaneLayoutState {
        if self.scopeID == scopeID { return state }
        return inMemoryStates[scopeID] ?? Self.loadState(
            from: fileURL,
            windowID: windowID,
            scopeID: scopeID
        )
    }

    func setDropTarget(_ target: PaneDropTarget?) {
        guard dropTarget != target else { return }
        dropTarget = target
    }

    func toggleZoom(groupID: String, paneID: String) {
        let zoom = ZoomedPane(groupID: groupID, paneID: paneID)
        zoomedPane = zoomedPane == zoom ? nil : zoom
    }

    func clearZoom() {
        guard zoomedPane != nil else { return }
        zoomedPane = nil
    }

    /// Repeated focus requests for the same pane remain observable because
    /// every request carries a fresh nonce.
    func requestFocus(groupID: String, paneID: String, sessionID: String) {
        focusRequest = FocusRequest(
            groupID: groupID,
            paneID: paneID,
            sessionID: sessionID,
            nonce: UUID()
        )
    }

    func clearFocusRequest() {
        guard focusRequest != nil else { return }
        focusRequest = nil
    }

    func setActivePane(groupID: String, paneID: String, sessionID: String?) {
        let active = ActivePane(
            groupID: groupID,
            paneID: paneID,
            sessionID: sessionID
        )
        guard activePane != active else { return }
        activePane = active
    }

    func clearActivePane() {
        guard activePane != nil else { return }
        activePane = nil
    }

    /// Consume only the request that actually reached a mounted pane. A late
    /// completion from an older focus attempt must not erase a newer nonce.
    @discardableResult
    func consumeFocusRequest(_ request: FocusRequest) -> Bool {
        guard focusRequest == request else { return false }
        focusRequest = nil
        return true
    }

    func setPendingReveal(groupID: String, paneID: String) {
        let reveal = PendingReveal(groupID: groupID, paneID: paneID)
        guard pendingReveal != reveal else { return }
        pendingReveal = reveal
    }

    /// Non-consuming peek used by a container's first layout pass.
    func pendingReveal(forGroupID groupID: String) -> PendingReveal? {
        guard pendingReveal?.groupID == groupID else { return nil }
        return pendingReveal
    }

    func pendingRevealPaneID(forGroupID groupID: String) -> String? {
        pendingReveal(forGroupID: groupID)?.paneID
    }

    /// Consumes exactly the reveal that mounted. A stale container cannot
    /// consume a newer group's or pane's animation request.
    @discardableResult
    func consumePendingReveal(groupID: String, paneID: String) -> Bool {
        guard pendingReveal == PendingReveal(groupID: groupID, paneID: paneID) else {
            return false
        }
        pendingReveal = nil
        return true
    }

    func clearPendingReveal() {
        guard pendingReveal != nil else { return }
        pendingReveal = nil
    }

    private func clearTransientPresentationState() {
        setDropTarget(nil)
        clearFocusRequest()
        clearPendingReveal()
        clearActivePane()
        clearZoom()
    }

    private func persist(_ state: PaneLayoutState) {
        // The lock file lives next to the storage file; on a fresh Controller
        // home the directory must exist before the lock can be opened.
        try? FileManager.default.createDirectory(
            at: fileURL.deletingLastPathComponent(),
            withIntermediateDirectories: true
        )
        _ = PresetStateFile.withExclusiveLock(on: fileURL) {
            persistLocked(state)
        }
    }

    private func persistLocked(_ state: PaneLayoutState) {
        var storage: StorageFile
        switch Self.loadStorageResult(from: fileURL) {
        case .missing:
            storage = StorageFile()
        case let .loaded(existing):
            // A future layout version may share this outer envelope while
            // using fields this build cannot preserve. Fail closed instead
            // of replacing that slot with an empty/current projection.
            // Version-1 slots are supported: they migrate on read and are
            // rewritten as version 2.
            let containsUnsupportedLayout = existing.windows.values
                .flatMap(\.values)
                .contains {
                    !DurablePaneLayout.supportedVersions.contains($0.version)
                }
            if containsUnsupportedLayout {
                return
            }
            storage = existing
        case .unreadable:
            // Never turn malformed or future-version Controller state into
            // data loss. The live layout still works; a later mutation can
            // retry after the file is repaired or upgraded.
            return
        }
        var scopes = storage.windows[windowID] ?? [:]
        scopes[scopeID] = DurablePaneLayout(state: state)
        storage.windows[windowID] = scopes
        Self.write(storage, to: fileURL)
    }

    private static func loadState(
        from fileURL: URL,
        windowID: String,
        scopeID: String
    ) -> PaneLayoutState {
        tryLoadState(from: fileURL, windowID: windowID, scopeID: scopeID)
            ?? PaneLayoutState()
    }

    private static func tryLoadState(
        from fileURL: URL,
        windowID: String,
        scopeID: String
    ) -> PaneLayoutState? {
        switch loadStorageResult(from: fileURL) {
        case .missing:
            return PaneLayoutState()
        case .unreadable:
            return nil
        case let .loaded(storage):
            guard let durable = storage.windows[windowID]?[scopeID],
                  DurablePaneLayout.supportedVersions.contains(durable.version)
            else {
                return PaneLayoutState()
            }
            return durable.restoredState()
        }
    }

    private static func loadStorage(from fileURL: URL) -> StorageFile? {
        guard case let .loaded(storage) = loadStorageResult(from: fileURL) else {
            return nil
        }
        return storage
    }

    private static func loadStorageResult(from fileURL: URL) -> StorageLoadResult {
        guard FileManager.default.fileExists(atPath: fileURL.path) else {
            return .missing
        }
        guard let data = try? Data(contentsOf: fileURL),
              let storage = try? JSONDecoder().decode(StorageFile.self, from: data),
              storage.version == StorageFile.currentVersion
        else {
            return .unreadable
        }
        return .loaded(storage)
    }

    private static func write(_ storage: StorageFile, to fileURL: URL) {
        do {
            try FileManager.default.createDirectory(
                at: fileURL.deletingLastPathComponent(),
                withIntermediateDirectories: true
            )
            let encoder = JSONEncoder()
            encoder.outputFormatting = [.prettyPrinted, .sortedKeys]
            let data = try encoder.encode(storage)
            try data.write(to: fileURL, options: .atomic)
        } catch {
            // Layout persistence is presentation-only and must not turn a pane
            // interaction into a user-visible failure. The live state remains
            // authoritative for this window and the next mutation retries.
        }
    }

    private struct StorageFile: Codable {
        static let currentVersion = 1

        var version = currentVersion
        var windows: [String: [String: DurablePaneLayout]] = [:]
    }

    private enum StorageLoadResult {
        case missing
        case loaded(StorageFile)
        case unreadable
    }

    private static func fileRevision(_ url: URL) -> TimeInterval? {
        (try? url.resourceValues(forKeys: [.contentModificationDateKey]))
            .flatMap(\.contentModificationDate)?
            .timeIntervalSince1970
    }
}
