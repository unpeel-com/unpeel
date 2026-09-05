//
//  UnpeelStore.swift
//  UnpeelNative
//
//  @MainActor store layer: reads projects from app-state.json, scans hosted
//  session manifests on a timer, derives busy state from output.bin growth,
//  and launches new sessions through unpeel-host.
//
//  Mostly read-only with respect to app-state.json; direct writes are limited
//  to shared settings the host must observe (`mcp_*`, `setup_completed`) plus
//  launch files (which the host deletes after reading).
//

import AppKit
import CUnpeelNativeBridge
import Combine
import CryptoKit
import Foundation
import SwiftUI
import UniformTypeIdentifiers
import UnpeelShared

/// UserDefaults keys that must be readable from nonisolated contexts too
/// (AdvancedDiagnostics.collect runs off the main actor).
enum NativeOverlay {
    /// [session id: custom title] — native renames. The Tauri app owns the
    /// manifest `label`/`custom_title`, so native renames live here and are
    /// merged over the manifest label at read time; entries are GC'd when
    /// the session dir disappears.
    static let sessionTitlesKey = "unpeel.native.sessionTitles"

    /// [Unpeel session id: provider conversation id] — captured from hook
    /// payloads (Claude forwards its `session_id`). Lets Restart resume the
    /// exact conversation via `--resume <id>` (see ResumeCommand). Entries are
    /// pruned with the session (pruneNativeState) and survive app restarts.
    static let providerSessionIDsKey = "unpeel.native.providerSessionIDs"

    /// [session id: restart recommendation token] — native dismissals for the
    /// compact restart bar. Tokens are stable capability/reason identifiers,
    /// so ordinary app rebuilds do not re-show dismissed recommendations.
    static let restartRecommendationDismissalsKey =
        "unpeel.native.restartRecommendationDismissals"

    /// [session id] — sessions the user opted into a "finished" push
    /// notification for (stored as an array of ids). Mac-side per-session flag,
    /// toggled from the desktop context menu or the phone's organize sheet;
    /// surfaced on `RemoteSessionSummary.notifyWhenDone` and read by the push
    /// dispatcher when a session settles. Pruned with the session.
    static let notifyWhenDoneKey = "unpeel.native.notifyWhenDone"

    /// [session id] — archived sessions (stored as an array of ids). Archive
    /// is the non-destructive "clear it out" verb: the hosted PTY is stopped
    /// but the session dir (manifest, output.bin, artifacts) stays on disk,
    /// so Resume brings the conversation back via ResumeCommand. Archived
    /// sessions form the fixed bottom sidebar section (newest first, sharing
    /// the inactive-preview cap with natural stops) and are all available from
    /// the project's dedicated archive page. GC'd when the session dir
    /// disappears; pruned on true removal.
    static let archivedSessionsKey = "unpeel.native.archivedSessions"

    /// [session id: ms epoch] — when the user explicitly archived the
    /// session. Drives archive-section ordering ("file it away" moves the
    /// row to the top of the archives). Automatic inactivity cleanup
    /// is deliberately unstamped so it never resurfaces above genuinely
    /// recent rows. Pruned with the archived flag.
    static let archivedAtKey = "unpeel.native.archivedAt"
}

struct SessionRestartRecommendation: Equatable {
    enum Action: Equatable {
        case resumeAgent
        case reloadTerminal

        var label: String {
            switch self {
            case .resumeAgent: return "Resume Agent"
            case .reloadTerminal: return "Reload Terminal"
            }
        }
    }

    let token: String
    let message: String
    /// Nil is an informational recommendation: the intent is queued, but no
    /// safe immediate action exists while the managed runtime is active.
    let action: Action?
}

/// Failure returned by the synchronous Host CLI receipt used for an in-place
/// agent restart. Eligibility/concurrency conflicts are 409; helper launch,
/// transport, signal, PTY, and ambiguous post-submit failures are 500.
struct ResumeAgentHostCommandFailure: Equatable, Sendable {
    let status: Int
    let message: String
}

/// Archive listings are fetched independently from the live Host bootstrap.
/// Keep their summaries in a separate page-scoped cache so a normal live
/// bootstrap refresh cannot erase the source metadata needed by Restore &
/// Resume. Session ids are Host-global, but tracking ownership by requested
/// project lets a refreshed archive page replace its rows without retaining
/// stale summaries.
struct RemoteArchivedSessionSummaryCache {
    private(set) var summariesByID: [String: RemoteSessionSummary] = [:]
    private var sessionIDsByProject: [String: Set<String>] = [:]

    subscript(sessionID: String) -> RemoteSessionSummary? {
        summariesByID[sessionID]
    }

    var sessionIDs: Set<String> {
        Set(summariesByID.keys)
    }

    mutating func replaceProject(
        _ projectID: String,
        summaries: [RemoteSessionSummary]
    ) {
        for sessionID in sessionIDsByProject[projectID] ?? [] {
            summariesByID.removeValue(forKey: sessionID)
        }
        let sessionIDs = Set(summaries.map(\.id))
        sessionIDsByProject[projectID] = sessionIDs
        for summary in summaries {
            summariesByID[summary.id] = summary
        }
    }

    mutating func retainProjects(_ projectIDs: Set<String>) {
        let removedProjectIDs = sessionIDsByProject.keys.filter {
            !projectIDs.contains($0)
        }
        for projectID in removedProjectIDs {
            for sessionID in sessionIDsByProject.removeValue(forKey: projectID) ?? [] {
                summariesByID.removeValue(forKey: sessionID)
            }
        }
    }

    mutating func removeAll() {
        summariesByID.removeAll()
        sessionIDsByProject.removeAll()
    }
}

/// RAII lease for one cross-process lifecycle/marker flock. Replacement
/// restart work crosses suspension points and actors, so the descriptor must
/// stay owned until teardown and replacement launch have both completed.
final class NativeSessionFileLockLease: @unchecked Sendable {
    private let lock = NSLock()
    private var descriptor: Int32

    init(descriptor: Int32) {
        self.descriptor = descriptor
    }

    func release() {
        lock.withLock {
            guard descriptor >= 0 else { return }
            _ = flock(descriptor, LOCK_UN)
            close(descriptor)
            descriptor = -1
        }
    }

    deinit {
        release()
    }
}

/// Generation binding decided before a hook may mutate provider metadata,
/// activity, unread state, history, or push state. The accepted generation is
/// exact provenance when the hook carried it, or a conservative binding for a
/// legacy event after a proven current-generation turn opener / bounded
/// compatibility guard.
enum HookRuntimeDecision: Equatable {
    case reject
    case accept(effectiveGeneration: UInt64?)
}

/// Temporary phone-driven terminal size for a session: the desktop pane is
/// letterboxed to this grid so the Mac and the phone render the same cells.
/// In-memory only — closing the app (or the banner's X) reverts to full size.
struct PhoneResizeOverride: Equatable {
    let cols: Int
    let rows: Int
}

struct SidebarSessionRevealRequest: Equatable {
    let sessionID: String
    let serial: Int
    /// Center the row (jump-to-session) vs minimal scroll that just brings
    /// it into view (follow a row that moved, e.g. after pinning).
    let centered: Bool
}

/// Host-side presentation for one active pairing grant. A successful pair
/// consumes the credential, so the UI must stop advertising that code at the
/// same moment and show completion instead. Matching by token prevents a late
/// completion callback from clearing a newer code the user already generated.
struct HostPairingPresentation: Equatable {
    let payload: RemotePairingPayload?
    let code: String?
    let completed: Bool

    static let idle = HostPairingPresentation(payload: nil, code: nil, completed: false)
    static let paired = HostPairingPresentation(payload: nil, code: nil, completed: true)

    static func active(_ payload: RemotePairingPayload) -> HostPairingPresentation {
        HostPairingPresentation(
            payload: payload,
            code: RemotePairingCode.encode(payload),
            completed: false
        )
    }

    func completing(token: String) -> HostPairingPresentation {
        guard payload?.token == token else { return self }
        return HostPairingPresentation(payload: nil, code: nil, completed: true)
    }
}

/// Selection has its own publication boundary. A session switch is extremely
/// hot UI state, while `UnpeelStore.objectWillChange` wakes every project node
/// and reconstructs the whole sidebar projection. Global observers subscribe
/// here; sidebar rows use per-id states so only the old and new chips repaint.
@MainActor
final class SessionSelectionState: ObservableObject {
    @Published private(set) var sessionID: String?

    private final class WeakRowState {
        weak var value: SessionSelectionRowState?

        init(_ value: SessionSelectionRowState) {
            self.value = value
        }
    }

    private var rowStates: [String: WeakRowState] = [:]

    func select(_ newSessionID: String?) {
        guard newSessionID != sessionID else { return }
        let oldSessionID = sessionID
        sessionID = newSessionID
        if let oldSessionID {
            rowStates[oldSessionID]?.value?.setSelected(false)
        }
        if let newSessionID {
            rowStates[newSessionID]?.value?.setSelected(true)
        }
        // Session ids can accumulate over a long-running app lifetime. Weak
        // entries keep row ownership local to SwiftUI; compact occasionally
        // without adding work to ordinary switches or large initial mounts.
        if rowStates.count > 1_024 {
            rowStates = rowStates.filter { $0.value.value != nil }
        }
    }

    func rowState(for sessionID: String) -> SessionSelectionRowState {
        if let existing = rowStates[sessionID]?.value {
            return existing
        }
        let state = SessionSelectionRowState(
            sessionID: sessionID,
            isSelected: self.sessionID == sessionID
        )
        rowStates[sessionID] = WeakRowState(state)
        return state
    }
}

@MainActor
final class SessionSelectionRowState: ObservableObject {
    let sessionID: String
    @Published private(set) var isSelected: Bool

    fileprivate init(sessionID: String, isSelected: Bool) {
        self.sessionID = sessionID
        self.isSelected = isSelected
    }

    fileprivate func setSelected(_ selected: Bool) {
        guard isSelected != selected else { return }
        isSelected = selected
    }
}

/// The asynchronously resolved git suffix belongs to the collapsed titlebar,
/// not to the app-wide store publication stream. Publishing it through
/// `UnpeelStore` made a completed `git symbolic-ref` invalidate every sidebar
/// project and row shortly after each cross-project session switch.
struct TitlebarBranchPresentation: Equatable {
    var name: String?
    var isWorktree: Bool
}

@MainActor
final class TitlebarBranchState: ObservableObject {
    @Published private(set) var presentation = TitlebarBranchPresentation(
        name: nil,
        isWorktree: false
    )

    func update(name: String?, isWorktree: Bool) {
        let next = TitlebarBranchPresentation(name: name, isWorktree: isWorktree)
        guard next != presentation else { return }
        presentation = next
    }
}

@MainActor
final class UnpeelStore: ObservableObject {
    /// Presentation state for the Controller window. Projects and Sessions
    /// remain Host-owned; pane grouping, geometry, focus, and reveal state do
    /// not cross this boundary into the session model.
    let paneLayoutController: PaneLayoutController
    @Published private(set) var nodes: [ProjectNode] = []
    let sessionSelection = SessionSelectionState()
    let titlebarBranchState = TitlebarBranchState()
    var selectedSessionID: String? {
        get { sessionSelection.sessionID }
        set {
            let oldValue = sessionSelection.sessionID
            sessionSelection.select(newValue)
            // Pane members collapse beneath one representative sidebar row.
            // Any generic selection source (notification, shortcut, restored
            // remote default) therefore navigates to that row while retaining
            // the user's exact member intent as a window focus request.
            if let requestedID = selectedSessionID,
               let group = validatedPaneGroup(containingSession: requestedID),
               let representativeID = group.representativeSessionID,
               representativeID != requestedID,
               let requestedPane = group.panes.first(where: {
                   $0.content.sessionID == requestedID
               }) {
                sessionSelection.select(representativeID)
                paneLayoutController.clearFocusRequest()
                paneLayoutController.requestFocus(
                    groupID: group.id,
                    paneID: requestedPane.id,
                    sessionID: requestedID
                )
            } else {
                // Any ordinary navigation supersedes an unconsumed exact-pane
                // request from a previous group or cold mount.
                paneLayoutController.clearFocusRequest()
            }
            // An empty ⌘D pane is a transient launch affordance, not part of
            // the durable pane layout. Leaving its group without choosing a
            // preset closes only the placeholder; launched panes remain
            // attached normally.
            if selectedSessionID != oldValue, let previous = oldValue {
                dismissPaneLaunchers(containing: previous)
            }
            // Remote scope: the same property drives selection, but the
            // backend owns it — forward to the runtime and skip every local
            // bookkeeping path (MRU, prewarm, local unread observation). A
            // stale local id can never smuggle a local Session into a
            // remote-scoped workspace.
            if selectedHostScope != .local {
                if let id = selectedSessionID {
                    guard remoteSessionsByID[id] != nil else {
                        sessionSelection.select(nil)
                        if oldValue != nil { refreshTitlebarBranch() }
                        return
                    }
                    if launcherProjectID != nil { launcherProjectID = nil }
                    if archivedProjectID != nil { archivedProjectID = nil }
                    if recentActivityVisible { recentActivityVisible = false }
                    if remoteHostRuntime.selectedSessionID != id {
                        remoteHostRuntime.selectSession(id)
                    }
                }
                // The title-strip branch is not local-host bookkeeping: a
                // scoped workspace still needs the suffix to follow the
                // selected Session's project/worktree.
                if selectedSessionID != oldValue {
                    refreshTitlebarBranch()
                }
                return
            }
            if localHostClientStarted,
               localHostProjectionReady,
               remoteHostRuntime.selectedSessionID != selectedSessionID {
                remoteHostRuntime.selectDirectDataPlaneSession(selectedSessionID)
            }
            // Selecting a real session dismisses the main-screen launcher
            // (launching a launcher tile selects the new session, which is
            // exactly how the picker gives way to the terminal). Do this even
            // when the id was already selected: clicking the highlighted row
            // is also a natural way to close the archive library.
            if selectedSessionID != nil {
                if launcherProjectID != nil { launcherProjectID = nil }
                if archivedProjectID != nil { archivedProjectID = nil }
                if recentActivityVisible { recentActivityVisible = false }
            }
            guard selectedSessionID != oldValue else { return }
            if let id = selectedSessionID { noteSessionMRU(id) }
            // Selection changes list membership only for a local inactive row
            // kept past the stopped/archive preview window. Remote lists arrive
            // pre-windowed by their Host. Preserve the memoized project lists
            // for the overwhelmingly common live-session switch.
            let windowedInactiveSessionIDs = Set(
                [oldValue, selectedSessionID].compactMap { $0 }.filter { id in
                    !archivingSessionIDs.contains(id)
                        && (archivedSessionIDs.contains(id)
                            || sessionsByID[id]?.isLive == false)
                }
            )
            if Self.selectionChangeAffectsSidebarLists(
                from: oldValue,
                to: selectedSessionID,
                windowedInactiveSessionIDs: windowedInactiveSessionIDs
            ) {
                invalidateSidebarLists()
            }
            handleObservationChanged()
            persistLocalSelectedSessionIfNeeded()
            // Keep the session we just left warm: switching back to it is
            // the most common next switch (A↔B ping-pong), and a prewarmed
            // pane stays mounted + ticking in WarmPaneHostView instead of
            // pausing detached. Deferred one runloop turn so the swap
            // container detaches the pane first — WarmPaneHostView refuses
            // to adopt a pane another superview still owns.
            if let previous = oldValue {
                DispatchQueue.main.async { [weak self] in
                    self?.prewarmSession(previous)
                }
            }
        }
    }
    @Published private(set) var projectsByID: [String: Project] = [:]

    /// Sessions with an unread badge (7px #60a5fa dot after the title).
    /// Mirrors `unreadSessionIds` in sessionState.ts plus the mark/clear
    /// rules from App.svelte's hook-event listener and sessionUnread.ts.
    @Published private(set) var unreadSessionIDs: Set<String> = []

    /// Persisted history feed (newest-last) behind the Recent panel and the
    /// titlebar bell. Backed by `<UNPEEL_HOME>/activity-log.jsonl`.
    @Published private(set) var activityLogEntries: [ActivityLogEntry] = []
    private let activityLog = ActivityLogStore()
    /// Last session status this run logged from, so rebuildTree can log the
    /// live → exited edge exactly once — and never for sessions that were
    /// already exited when the app started.
    private var activityLoggedStatuses: [String: SessionStatus] = [:]

    /// Whether the "All recent" main-pane page (RecentActivityView) is
    /// showing — a content-pane swap like `archivedProjectID`, opened from
    /// the activity dropdowns' footer link. Deliberately not persisted.
    @Published var recentActivityVisible = false {
        didSet {
            if recentActivityVisible, let selectedSessionID {
                dismissPaneLaunchers(containing: selectedSessionID)
            }
            if recentActivityVisible != oldValue { handleObservationChanged() }
        }
    }

    /// Sessions the user opted into a "finished" push notification for
    /// (`NativeOverlay.notifyWhenDoneKey`). Surfaced on
    /// `RemoteSessionSummary.notifyWhenDone` and read by the push dispatcher
    /// when a session settles (Stop). Persisted; pruned with the session.
    @Published private(set) var notifyWhenDoneSessionIDs: Set<String> =
        Set(AppDefaults.shared.stringArray(forKey: NativeOverlay.notifyWhenDoneKey) ?? [])

    /// Archived sessions (`NativeOverlay.archivedSessionsKey`): hidden from
    /// the regular sidebar lists (and the phone snapshot) and opened from the
    /// owning project's context menu in the main pane. Persisted; GC'd on
    /// rescan when the session dir is gone, pruned on removal.
    @Published private(set) var archivedSessionIDs: Set<String> =
        Set(AppDefaults.shared.stringArray(forKey: NativeOverlay.archivedSessionsKey) ?? []) {
        didSet { if archivedSessionIDs != oldValue { invalidateSidebarLists() } }
    }
    /// Deduplicates asynchronous stop/reap work, including recovery after an
    /// app interruption between persisting the archive flag and stopping the
    /// host.
    private var stoppingArchivedSessionIDs: Set<String> = []

    /// Archives whose live host is still shutting down: the sidebar keeps
    /// these rows visible — muted, with a spinner — until the stop finishes,
    /// instead of vanishing them mid-click. In-memory only (a relaunch's
    /// recovery stop needs no visible row).
    @Published private(set) var archivingSessionIDs: Set<String> = [] {
        didSet { if archivingSessionIDs != oldValue { invalidateSidebarLists() } }
    }

    /// When the user explicitly archived each session (ms epoch,
    /// `NativeOverlay.archivedAtKey`): a stamped row sorts to the top of the
    /// archive section. Automatic inactivity archives never stamp. Pruned
    /// alongside the archived flag.
    private var archivedAtBySession: [String: Int64] =
        (AppDefaults.shared.dictionary(forKey: NativeOverlay.archivedAtKey) ?? [:])
            .compactMapValues { ($0 as? NSNumber)?.int64Value }

    /// Project ids whose session lists are expanded in the sidebar.
    /// Persisted (unlike the Svelte `expandedProjectIds` store, which reset
    /// to all-collapsed on every launch) so reopening the app restores which
    /// folders were open. Entries are pruned on project removal.
    @Published var expandedProjectIDs: Set<String> =
        Set(AppDefaults.shared.stringArray(forKey: UnpeelStore.expandedProjectsKey) ?? []) {
        didSet {
            guard expandedProjectIDs != oldValue else { return }
            if expandedProjectIDs.isEmpty, expandedProjectsStorageKey == Self.expandedProjectsKey {
                AppDefaults.shared.removeObject(forKey: expandedProjectsStorageKey)
            } else {
                // Per-Host keys persist an explicit empty array: "user collapsed
                // everything" must stay distinguishable from "never visited"
                // (which triggers the open-all-roots first-visit default).
                AppDefaults.shared.set(
                    Array(expandedProjectIDs), forKey: expandedProjectsStorageKey
                )
            }
        }
    }

    /// Where `expandedProjectIDs` persists: the shared local key, or a
    /// per-Host key while a remote Host is selected — expansion is
    /// Controller-local view state, remembered separately per Host.
    private var expandedProjectsStorageKey = UnpeelStore.expandedProjectsKey

    /// Native-only project id -> `ProjectFolderColor.rawValue`.
    @Published private(set) var projectFolderColorIDs: [String: String] = [:]

    /// Sidebar inactive preview: active sessions always render; below them at
    /// most this many naturally stopped or archived rows show. Pinned rows
    /// don't count against the window. User-selectable — shared
    /// with the TUI via the compatibility key `sidebar_stopped_limit` in
    /// app-state.json; absent key = five, explicit 0 = no inactive preview.
    static let sidebarStoppedLimitOptions = [0, 3, 5, 10, 15, 25]
    static let defaultSidebarStoppedLimit = 5
    @Published private(set) var sidebarVisibleSessionLimit = UnpeelStore.defaultSidebarStoppedLimit {
        didSet { if sidebarVisibleSessionLimit != oldValue { invalidateSidebarLists() } }
    }

    /// Session ids pinned visible past the inactive-preview window — set when
    /// the app must bring a hidden stopped or archived row back into the sidebar
    /// (restore from archive, reveal on select). In-memory only; pruned with
    /// the session on rescan.
    @Published private(set) var sidebarKeepVisibleSessionIDs: Set<String> = [] {
        didSet { if sidebarKeepVisibleSessionIDs != oldValue { invalidateSidebarLists() } }
    }

    /// Recency stamp applied when a Session is unpinned so the row re-enters
    /// its stopped/archive block at the top (right below the live rows)
    /// instead of falling back to its old lifecycle position — which, for an
    /// old archived row, pushed it past the preview window and made it look
    /// like unpin closed the Session. In-memory only, pruned with the session
    /// on rescan, exactly like `sidebarKeepVisibleSessionIDs`.
    @Published private(set) var sidebarUnpinRecencyBump: [String: Int64] = [:] {
        didSet { if sidebarUnpinRecencyBump != oldValue { invalidateSidebarLists() } }
    }

    /// Collapse-all's reset of the keep-visible pins (the per-project
    /// collapse clears its own inside `toggleProjectExpanded`).
    func clearSidebarKeepVisiblePins() {
        guard !sidebarKeepVisibleSessionIDs.isEmpty else { return }
        sidebarKeepVisibleSessionIDs = []
    }

    /// Collapse every expanded sidebar folder (Session ▸ menu and ⌥⌘B).
    /// Also drops the keep-visible pins, like the per-project collapse does.
    func collapseAllSidebarFolders() {
        withAnimation(SidebarMotion.accordionClose) {
            expandedProjectIDs = []
            clearSidebarKeepVisiblePins()
        }
    }

    /// Explicit request for the sidebar to bring a session row into view.
    /// Selection alone is not enough because the target can be behind a
    /// collapsed project, a worktrees pane switch, or the "Show N more" cap.
    @Published private(set) var sidebarSessionRevealRequest: SidebarSessionRevealRequest?
    private var sidebarSessionRevealSerial = 0

    /// Sidebar collapsed (hidden) state. The Svelte app does not persist
    /// this; the native app does (requested), alongside unpeel.sidebar.width.
    @Published var sidebarCollapsed: Bool {
        didSet {
            AppDefaults.shared.set(sidebarCollapsed, forKey: Self.sidebarCollapsedKey)
        }
    }

    /// Whether to surface a session's attention badge when the host detects an
    /// agent-drawn select menu waiting for a choice (Claude/Codex numbered
    /// prompts, which fire no lifecycle hook). On by default; a rescan re-derives
    /// status when this flips, so toggling it applies live. See the host's
    /// `menu_prompt_active` manifest flag.
    @Published var menuAttentionDetectionEnabled: Bool {
        didSet {
            guard oldValue != menuAttentionDetectionEnabled else { return }
            // Baseline re-resolves (revert, inherited-value reload) must not
            // materialize the inherited value as an own override.
            if !suppressMenuAttentionPersistence {
                AppDefaults.shared.set(
                    menuAttentionDetectionEnabled, forKey: Self.menuAttentionDetectionKey
                )
            }
            rescan()
        }
    }

    private var suppressMenuAttentionPersistence = false

    /// The effective menu-attention setting: this workspace's own value,
    /// else the default workspace's (Decision 4 generalized — the shared
    /// `.standard` domain is the local-filesystem baseline), else on.
    static func resolveMenuAttentionDetection() -> Bool {
        if let own = AppDefaults.shared.object(
            forKey: menuAttentionDetectionKey
        ) as? Bool {
            return own
        }
        if !UnpeelWorkspaceContext.isDefaultInstance,
           let inherited = UserDefaults.standard.object(
               forKey: menuAttentionDetectionKey
           ) as? Bool {
            return inherited
        }
        return true
    }

    /// Whether this workspace records its own menu-attention value — the
    /// revert button's enablement.
    static var hasOwnMenuAttentionSetting: Bool {
        AppDefaults.shared.object(forKey: menuAttentionDetectionKey) != nil
    }

    /// Decision 4's revert for Notifications on THIS instance: drop the own
    /// value and re-resolve from the inherited baseline without persisting.
    func revertNotificationsToInheritedBaseline() {
        AppDefaults.shared.removeObject(forKey: Self.menuAttentionDetectionKey)
        suppressMenuAttentionPersistence = true
        menuAttentionDetectionEnabled = Self.resolveMenuAttentionDetection()
        suppressMenuAttentionPersistence = false
    }

    /// Decision 4's revert for Experimental on THIS instance: drop every own
    /// flag value and republish the inherited resolution.
    func revertExperimentalToInheritedBaseline() {
        UnpeelFeatureFlags.revertToInheritedBaseline()
        enabledExperimentalKeys = Set(
            ExperimentalFeature.all
                .filter { UnpeelFeatureFlags.isEnabled($0) }
                .map(\.key)
        )
    }

    /// Whether the terminal title bar shows the session gallery chip and the
    /// Session ▸ Take Screenshot… flow that feeds it (Appearance ▸ "Session
    /// gallery"). Off by default — users with their own screenshot tooling
    /// keep a plain title bar. Desktop-only: the phone gallery and the
    /// artifact dirs it reads are unaffected.
    @Published var showSessionGallery: Bool {
        didSet {
            guard oldValue != showSessionGallery else { return }
            AppDefaults.shared.set(showSessionGallery, forKey: Self.showSessionGalleryKey)
        }
    }

    /// Whether provider-created linked worktrees — the checkouts Claude
    /// Code, Conductor, and similar tools mint for themselves — are adopted
    /// into the sidebar as child folders (Settings ▸ Worktrees). Off by
    /// default: those checkouts are the agent's scratch space, not something
    /// the user filed. Safe both ways because auto-discovered records are
    /// reconstructible: off purges them, on rediscovers them. Worktrees the
    /// user registered through Unpeel's own UI are never touched.
    @Published var showAgentWorktrees: Bool {
        didSet {
            guard oldValue != showAgentWorktrees else { return }
            AppDefaults.shared.set(
                showAgentWorktrees, forKey: Self.showAgentWorkspacesKey
            )
            refreshAgentWorktreeAdoption()
        }
    }

    /// Apply a "Show agent worktrees" flip now, in both directions and in
    /// both Local modes. The compatibility Host adopts/purges inside its
    /// rescan; as a Host client `rescan()` only asks the worker for a fresh
    /// snapshot and never reaches that sync, so the flip used to wait for
    /// the next launch. Run the sync directly against the projected tree:
    /// the purge/adoption mirrors into app-state.json and announces, the
    /// worker republishes, and the projection rebuilds the rows.
    private func refreshAgentWorktreeAdoption() {
        // Skip the discovery throttle so the flip applies right away.
        lastLinkedWorktreeDiscoveryAt = .distantPast
        guard !shouldApplyLocalDiskProjection else {
            scheduleRescan(after: 0)
            return
        }
        _ = syncLinkedWorktreeProjects(from: Array(displayProjectsByID.values))
        // The mirror write pinged the worker over the state bus; give it a
        // beat to re-read before asking for the republished snapshot.
        scheduleRescan(after: 0.25)
    }

    /// What the app-wide ⌘T shortcut opens. Explicit New Terminal actions
    /// elsewhere stay literal; only the shortcut/menu key equivalent follows
    /// this Appearance setting.
    @Published var commandTAction: CommandTAction {
        didSet {
            guard oldValue != commandTAction else { return }
            AppDefaults.shared.set(commandTAction.rawValue, forKey: Self.commandTActionKey)
        }
    }

    /// Whether the window is in macOS fullscreen. In fullscreen the traffic
    /// lights are hidden, so the titlebar toggle slides flush to the left
    /// edge instead of clearing the (now-absent) traffic lights. Driven by
    /// the window-delegate fullscreen notifications in `AppDelegate`.
    @Published var windowIsFullScreen = false

    /// When set, the main content area shows the session launcher (a
    /// pick-a-tool screen) for this project instead of a terminal/empty
    /// state. Driven by the Finder "New Unpeel Session Here" service and
    /// the empty state. Pure native UI state; never persisted. Cleared the
    /// moment a real session is selected (see `selectedSessionID`).
    @Published var launcherProjectID: String?

    /// Project whose archived-session library fills the main content area.
    /// Opened from the project row's context menu; selecting a session,
    /// opening Settings, or opening the launcher dismisses it.
    @Published var archivedProjectID: String? {
        didSet {
            guard archivedProjectID != oldValue else { return }
            if archivedProjectID != nil, let selectedSessionID {
                dismissPaneLaunchers(containing: selectedSessionID)
            }
            // The Host archive endpoint is a page-scoped data source. Drop
            // the old page immediately so a late response cannot make an
            // archived row (or its resume metadata) actionable after close
            // or after switching projects.
            remoteArchivePageGeneration &+= 1
            remoteArchivedByProject = [:]
            remoteArchivedSummaryCache.removeAll()
            handleObservationChanged()
        }
    }

    /// Display pins per project id, newest-first, pruned to sessions that
    /// still exist. Merged from app-state.json (Tauri-owned, read-only) and
    /// native-side overrides in UserDefaults; native overrides win.
    @Published private(set) var pinnedByProject: [String: [PinnedSidebarSession]] = [:]

    /// Quick-preset strip contents (same for every project — the native app
    /// is global-presets-only): starred presets grouped by CLI in flat-list
    /// order. A single-preset group is a plain launch chip; 2+ starred
    /// presets of one CLI render as a dropdown chip.
    /// Refreshed on rescan, so edits to app-state.json show up promptly.
    @Published private(set) var quickPresetGroups: [QuickPresetGroup] = []

    /// Flattened quick-strip launch targets (each group's topmost preset),
    /// plus the blank-terminal preset — for single-click surfaces like ⌘N.
    @Published private(set) var quickPresets: [Preset] = [.newTerminal]

    /// All global presets, in flat list order. The property name remains for
    /// source compatibility; presets are now present-or-deleted, so every
    /// loaded preset is enabled.
    @Published private(set) var enabledPresets: [Preset] = []

    /// All global presets, in flat list order, backing launch surfaces. The
    /// property name remains for source compatibility; launch choices are no
    /// longer filtered by PATH availability.
    @Published private(set) var availablePresets: [Preset] = []

    /// Flat preset display order (preset ids, native UserDefaults overlay).
    /// Empty → app-state.json order. Ids missing from the saved order (new
    /// presets) append at the end; the order also decides each CLI's default
    /// preset (topmost enabled preset wins — see `defaultPreset(for:)`).
    /// Legacy: unread once `presetsInSharedFile` is true.
    @Published private(set) var presetOrder: [String] = []

    /// True once app-state.json carries the migrated preset truth (the
    /// `native_preset_overlay_migrated` marker): every preset mutator then
    /// writes the shared file — which the TUI edits too — instead of the
    /// legacy UserDefaults overlay. Set during rescan.
    private var presetsInSharedFile = false

    /// `code_editor` from app-state.json (state.rs default "code"), with a
    /// native UserDefaults overlay. Backs "Open in <Editor>" and the
    /// titlebar open button.
    @Published private(set) var codeEditor = "code"

    /// Effective appearance mode (Settings → Appearance). Merge rule like
    /// pins/presets: the native UserDefaults overlay wins; until the user
    /// picks a mode natively, this follows the Tauri app-state.json `theme`
    /// (refreshed on rescan). Drives NSApp.appearance — every dynamic
    /// Theme color, the vibrancy materials, and the Ghostty surfaces'
    /// light/dark configs follow from that.
    @Published private(set) var themePreference: ThemePreference = .system

    /// Workspace chrome tint (Settings → Appearance → App color). Native-only
    /// UserDefaults overlay, so each workspace instance keeps its own color;
    /// mirrored into `Theme.appTintHue` for the resolve-time wash and
    /// advertised to paired phones via the /mobile bootstrap.
    @Published private(set) var appTint: AppTint = .none

    /// Per-session Unpeel Sessions MCP access overrides, keyed by session id.
    /// Read from app-state.json on rescan; written back by `setSessionAccess`.
    /// The host reads the same `mcp_orchestrators` field, so this is a direct
    /// (not overlay) setting. A session absent from the map uses the default
    /// grant (`McpGrant.default` — Member at project reach).
    @Published private(set) var mcpOrchestrators: [String: McpGrant] = [:]

    /// App-wide default Sessions MCP access for sessions without an explicit
    /// override (the `mcp_default_access` field). Read from / written back to
    /// app-state.json so the host honors it too.
    @Published private(set) var mcpDefaultAccess: SessionAccessLevel = .read

    /// App-wide policy for Sessions MCP writes to any other session
    /// (`mcp_nonchild_write_access`). Read from / written back to
    /// app-state.json; the host re-reads it per tool call, so changes apply
    /// live. The persisted key keeps its historical name for compatibility.
    @Published private(set) var mcpNonChildWriteAccess: McpNonChildWriteAccess = .ask

    /// User-approved write pairs (`mcp_write_approvals`), caller
    /// session id → approved target session ids. Written when the user answers
    /// the approval dialog (and revoked from Settings ▸ Sessions MCP); the host
    /// reads it per write. Pruned with sessions, re-pointed across restarts.
    @Published private(set) var mcpWriteApprovals: [String: [String]] = [:]

    /// Remembered caller/App launch approvals. The MCP host reads the same
    /// `mcp_app_open_approvals` map before requesting an App panel.
    @Published private(set) var mcpAppOpenApprovals: [String: [String]] = [:]

    /// Unified FIFO of pending ask-mode approvals (MCPApprovalCenter.swift):
    /// bridge requests blocked on an answer. Published because two surfaces
    /// render it — the in-session desktop overlay and the paired-phone
    /// bootstrap snapshot; either may answer, first one wins. Mutated only by
    /// enqueueMcpApproval/answerMcpApproval (internal set for the same-module
    /// extension in MCPApprovalCenter.swift).
    @Published var pendingMcpApprovals: [PendingMcpApproval] = [] {
        didSet { invalidateSidebarLists() }
    }
    /// Approval rows whose blocking request and answer authority live in the
    /// canonical Rust workspace worker. The native app presents them only;
    /// Allow/Deny routes back over the same local Host connection.
    var hostOwnedMcpApprovalIDs: Set<String> = []
    var hostMcpApprovalMessages: [String: (title: String, body: String)] = [:]
    var hostMcpApprovalAnswersInFlight: Set<String> = []
    /// The floating panel for the computer-permissions (TCC) nudge.
    let computerNudgePanel = FloatingPromptPanelController()
    /// Missing-permission sets (sorted, "|"-joined) already alerted about this
    /// app run — the grant-prompt nudge fires once per distinct set.
    var shownComputerPermissionNudges: Set<String> = []

    /// App-wide Browser Access (the `browser_default_access` field). Defaults
    /// to on — the browser is an isolated per-session profile (no access to the
    /// user's logins) and agents already have full shell, so it adds
    /// visibility, not privilege. Settings ▸ Browser ▸ Off is the master
    /// disable. There is no per-session override; this is the single switch.
    @Published private(set) var browserDefaultAccess: BrowserAccess = .on

    /// App-wide Computer MCP access (`computer_default_access`) and the
    /// remembered per-session approvals (`computer_approvals`). The unified
    /// MCP server reads both from app-state.json per call, so changes apply
    /// live; the app owns the writes.
    @Published private(set) var computerDefaultAccess: ComputerAccess = .ask
    @Published private(set) var computerApprovals: [String] = []
    /// Remembered per-session browser approvals (`browser_approvals`), used
    /// only while `browserDefaultAccess == .ask`.
    @Published private(set) var browserApprovals: [String] = []
    /// Whether sessions may create Unpeel-managed worktrees
    /// (`mcp_worktree_access`, Settings ▸ Sessions use). Default off.
    @Published private(set) var mcpWorktreeAccess = false
    /// Whether Browser MCP screenshots are added to the Session gallery by
    /// default (`mcp_auto_add_browser_screenshots`). Default on to preserve
    /// the original gallery behavior.
    @Published private(set) var mcpAutoAddBrowserScreenshots = true

    /// Unpeel Link profile (`profile_display_name` / `profile_avatar`): the
    /// nickname and emoji avatar presence surfaces show for this person when
    /// several controllers share a Host. The TUI edits the same
    /// app-state.json keys, so writes go through the locked shared-file
    /// editor and edits from either frontend land in both.
    @Published private(set) var profileDisplayName = ""
    @Published private(set) var profileAvatar = ""

    /// App-wide Browser MCP engine options (`browser_settings`): window
    /// visibility, site rules, browsing-data mode, custom executable. The
    /// host reads them per tool call, so changes apply to the agent's next
    /// browser action without a restart.
    @Published private(set) var browserSettings = BrowserSettings()

    /// App-wide transcript rendering options (`transcript_settings`): which
    /// content types the Markdown transcript includes and how many entries.
    /// The host reads them from app-state.json when building the transcript for
    /// "Copy Transcript" and the Sessions MCP `read_transcript` tool, so changes
    /// apply on the next copy / read without a restart.
    @Published private(set) var transcriptSettings = TranscriptSettings()

    /// Live sessions that should be restarted to pick up a required
    /// `unpeel-host` protocol. Populated from manifest host_protocol_version
    /// during rescans; native dismissals hide individual recommendations.
    @Published private(set) var restartRecommendations: [String: SessionRestartRecommendation] =
        [:]

    /// Loopback URLs each live session currently serves as a browsable page,
    /// from the host's `detected_local_urls` manifest field (printed on the
    /// session's screen, then HTTP-probed live by the host; dead servers are
    /// removed host-side). Keyed by session id; the titlebar chip aggregates
    /// per project family via `localSiteURLs(forProjectFamilyOf:)`.
    @Published private(set) var detectedLocalURLs: [String: [String]] = [:]

    /// Display-layer verdicts for detected local-site URLs, keyed by URL.
    /// Session hosts are long-lived processes running whatever detection
    /// code they started with, so the chip re-verifies every manifest URL
    /// against the CURRENT probe rules (via `unpeel-host
    /// __check_local_url__`, keeping the "is this an openable site" logic
    /// single-sourced in Rust) before showing it.
    private var localURLVerdicts: [String: (ok: Bool, at: Date)] = [:]
    private var localURLChecksInFlight: Set<String> = []
    /// Bumped when a verdict lands so SwiftUI re-renders the chip.
    @Published private var localURLVerdictRevision = 0
    /// URLs already announced with a toast; cleared when the site goes down
    /// so a dev server restart announces again.
    private var announcedLocalURLs: Set<String> = []

    /// Current-rules verdict for one manifest URL, from the async cache. An
    /// unknown URL kicks a background probe and stays hidden until it
    /// passes; verdicts refresh every few seconds so a server that dies —
    /// or starts working — converges quickly.
    private func localURLVerdict(_ url: String) -> Bool {
        let cached = localURLVerdicts[url]
        let fresh = cached.map { Date().timeIntervalSince($0.at) < 5 } ?? false
        if !fresh, !localURLChecksInFlight.contains(url) {
            localURLChecksInFlight.insert(url)
            DispatchQueue.global(qos: .utility).async { [weak self] in
                let process = Process()
                process.executableURL = URL(fileURLWithPath: LaunchConfig.hostBinary)
                process.arguments = ["__check_local_url__", url]
                let pipe = Pipe()
                process.standardOutput = pipe
                process.standardError = FileHandle.nullDevice
                var ok = false
                if (try? process.run()) != nil {
                    let data = pipe.fileHandleForReading.readDataToEndOfFile()
                    process.waitUntilExit()
                    ok = String(data: data, encoding: .utf8)?
                        .contains("\ttrue") ?? false
                }
                DispatchQueue.main.async {
                    guard let self else { return }
                    self.localURLVerdicts[url] = (ok, Date())
                    self.localURLChecksInFlight.remove(url)
                    self.localURLVerdictRevision &+= 1
                    self.announceLocalURLIfNew(url, ok: ok)
                }
            }
        }
        return cached?.ok ?? false
    }

    /// Toast the first time a local site verifies live (mirrors the phone
    /// "connected" toast; tap opens the site). Scoped to the currently shown
    /// session's project family so background projects don't spam, and
    /// re-armed when the site goes down so a dev-server restart announces
    /// again.
    private func announceLocalURLIfNew(_ url: String, ok: Bool) {
        guard ok else {
            announcedLocalURLs.remove(url)
            return
        }
        guard !announcedLocalURLs.contains(url),
              let session = selectedSession,
              localSiteURLs(forProjectFamilyOf: session.projectID).contains(url)
        else { return }
        announcedLocalURLs.insert(url)
        let compact = url
            .replacingOccurrences(of: "https://", with: "")
            .replacingOccurrences(of: "http://", with: "")
            .prefix(while: { $0 != "/" })
        ToastCenter.shared.show(
            "\(compact) is running — tap to open",
            chromeIcon: .globe
        ) {
            LocalSiteMenu.open(url)
        }
    }

    /// Union of detected local-site URLs across every live session in the
    /// same project family (the top-level project plus its groups and
    /// worktrees) — the dev server usually runs in one session while the
    /// user watches another, so the chip is project-scoped, not
    /// session-scoped. Stable order: session creation time, then URL.
    func localSiteURLs(forProjectFamilyOf projectID: String) -> [String] {
        func rootID(_ id: String) -> String {
            var current = id
            var hops = 0
            while let parent = projectsByID[current]?.parentProjectID, hops < 8 {
                current = parent
                hops += 1
            }
            return current
        }
        let familyRoot = rootID(projectID)
        let members = detectedLocalURLs.compactMap { sessionID, urls -> (Int64, [String])? in
            guard let entry = sessionsByID[sessionID],
                  rootID(entry.projectID) == familyRoot
            else { return nil }
            return (entry.createdAt, urls)
        }
        // One row per server: group by origin and keep the URL closest to
        // the parent — a deep link survives only while no parent URL exists
        // for the same origin. Then each survivor must pass the
        // current-rules probe.
        var byOrigin: [(origin: String, url: String)] = []
        for (_, urls) in members.sorted(by: { $0.0 < $1.0 }) {
            for url in urls {
                guard let origin = Self.urlOrigin(url) else { continue }
                if let index = byOrigin.firstIndex(where: { $0.origin == origin }) {
                    if Self.urlPathLength(url) < Self.urlPathLength(byOrigin[index].url) {
                        byOrigin[index].url = url
                    }
                } else {
                    byOrigin.append((origin, url))
                }
            }
        }
        return byOrigin.map(\.url).filter { localURLVerdict($0) }
    }

    /// "http://localhost:5173/whatever" → "http://localhost:5173/".
    private static func urlOrigin(_ url: String) -> String? {
        guard let schemeEnd = url.range(of: "://") else { return nil }
        let rest = url[schemeEnd.upperBound...]
        let authority = rest.prefix(while: { $0 != "/" })
        return url[..<schemeEnd.upperBound] + authority + "/"
    }

    /// Path length after the authority; bare origin and "/" both count 0.
    private static func urlPathLength(_ url: String) -> Int {
        guard let schemeEnd = url.range(of: "://") else { return .max }
        let rest = url[schemeEnd.upperBound...]
        guard let slash = rest.firstIndex(of: "/") else { return 0 }
        return rest.distance(from: slash, to: rest.endIndex) - 1
    }

    /// Sessions whose restart-with-resume relaunch failed because the
    /// provider's conversation no longer exists on disk (e.g. Claude Code's
    /// auto-cleanup deleted the transcript) — the CLI printed its
    /// "conversation not found" error and exited to a bare shell. Keyed by the
    /// REPLACEMENT session id; drives ResumeFailedBar's one-click fresh
    /// relaunch. In-memory only: detection runs in the seconds after a
    /// restart, so it has nothing to survive an app relaunch.
    @Published private(set) var resumeFailures: Set<String> = []

    /// Post-restart output watchers behind `resumeFailures`, keyed by the
    /// replacement session id so removal/restart can cancel them.
    private var resumeFailureWatchers: [String: Task<Void, Never>] = [:]
    /// Same-Session Resume Agent can replace a watcher while its cancelled
    /// predecessor is still unwinding. Tokens keep the predecessor from
    /// removing or publishing results into the newer generation's watcher.
    private var resumeFailureWatcherTokens: [String: UUID] = [:]

    /// Sessions whose desktop terminal is temporarily letterboxed to a
    /// phone's grid (set over the mobile/dev bridge). Cleared by the
    /// terminal banner's X, by the phone, or when the session goes away.
    @Published private(set) var phoneResizeOverrides: [String: PhoneResizeOverride] = [:]

    /// Project ids explicitly blocked from MCP (app-state.json
    /// `mcp_blocked_projects`). Keyed by id so overlay-only/worktree projects
    /// are blockable too. `projectMcpBlocked` adds parent-chain inheritance.
    @Published private(set) var mcpBlockedProjectIDs: Set<String> = []

    /// Groups (project/group/worktree id) whose sessions sort by date
    /// (recently updated first) instead of the manual drag order —
    /// app-state.json `session_sort_modes`, shared with the TUI. Date sort
    /// disables drag re-ordering for the group; the stored manual order
    /// survives a switch back to custom.
    @Published private(set) var dateSortedProjectIDs: Set<String> = []

    /// Settings ▸ Advanced auto-stop-and-archive: sessions CONTINUOUSLY idle
    /// for this many minutes (0 = off) are stopped and archived in one motion
    /// — the same `archiveSession` verb as "Stop and archive" in the sidebar,
    /// so nothing is deleted and Restore + Restart resumes the conversation.
    /// Shared with the TUI via `auto_stop_archive_minutes` in app-state.json.
    @Published private(set) var autoStopArchiveMinutes = 0

    /// Settings ▸ Appearance session-title driver — `session_title_mode` in
    /// app-state.json, shared with the TUI and read live by every session
    /// host (the host applies titles; the app only edits the knob).
    @Published private(set) var sessionTitleMode: SessionTitleMode = .agent

    /// Settings open (App.svelte shellView kind 'settings'). Natively the
    /// app layout stays mounted: the sidebar list area slides to the
    /// settings nav (SettingsSidebarPanel) and the content pane swaps to
    /// the active settings panel (SettingsContentHost) — the retained
    /// Ghostty surfaces are never animated or hidden behind a fade.
    /// Opened by the footer gear and ⌘,. Toggling re-runs the unread
    /// reconciliation because the selected session stops being "observed"
    /// while settings covers the content pane.
    @Published var settingsVisible = false {
        didSet {
            if settingsVisible, let selectedSessionID {
                dismissPaneLaunchers(containing: selectedSessionID)
            }
            if settingsVisible != oldValue { handleObservationChanged() }
        }
    }

    /// Active settings tab (App.svelte shellView.tab); defaults to the
    /// first tab in the nav.
    @Published var settingsTab: SettingsTab = .presets

    /// Keys of the experimental features (Settings ▸ Experimental) that are
    /// currently enabled. Seeded from the registry so an env override or a
    /// stored preference is reflected at launch; publishing it lets the
    /// sidebar's worktree gates re-evaluate live when a toggle flips.
    @Published private(set) var enabledExperimentalKeys: Set<String> =
        Set(ExperimentalFeature.all.filter { UnpeelFeatureFlags.isEnabled($0) }.map(\.key))

    /// This app's Host-side remote-control server. Mobile was its first
    /// Controller, so the shipped implementation and routes retain legacy
    /// mobile names; the app-facing model is generic Host/Controller pairing.
    /// It binds on the LAN and requires a per-device bearer token for every
    /// terminal endpoint. Pairing codes are short-lived and one-time.
    let hostManagement = HostManagementState()
    private(set) var hostServerEndpoint: URL? {
        get { hostManagement.value.endpoint }
        set { hostManagement.update(endpoint: newValue) }
    }
    private(set) var hostServerError: String? {
        get { hostManagement.value.error }
        set { hostManagement.update(error: newValue) }
    }
    @Published private(set) var hostPairingPresentation = HostPairingPresentation.idle
    @Published private(set) var remoteHostPairingPresentation = HostPairingPresentation.idle
    @Published private(set) var remoteHostPairingError: String?
    /// Pairing a device with the SELECTED local workspace from this window
    /// (one pairing = one workspace): the sibling instance mints the code
    /// over the MCP-token loopback bridge and this window only displays it.
    @Published private(set) var scopedWorkspacePairingPresentation = HostPairingPresentation.idle
    @Published private(set) var scopedWorkspacePairingError: String?
    var pairedControllers: [RemotePairedDeviceSummary] { hostManagement.value.devices }
    /// Native credential adapter for the worker's Keychain/push/Relay
    /// platform callbacks. Authorization authority stays with the worker's
    /// locked `devices.json`; this store only mirrors the native halves.
    private lazy var platformPairingStore = MobilePairingStore(macID: localHostID)
    /// Polls only the small same-user management snapshot. Session/sidebar
    /// state has its own persistent RemoteHostRuntime connection; this task
    /// keeps Settings and one-time pairing presentation in sync with the same
    /// worker without reading or mutating its authorization files directly.
    private var localHostControlRefreshTask: Task<Void, Never>?
    private var remoteHostPairingTask: Task<Void, Never>?
    private var remoteHostPairingGeneration: UInt64 = 0
    /// A Controller transport, never a Host listener: it accepts one sealed
    /// phone envelope and forwards it over the selected remote Host runtime.
    private var controllerPairingProxy: ControllerPairingProxy?
    private var remoteHostPairingProxyID: String?
    private var remoteHostPairingHostID: String?
    private var scopedWorkspacePairingTask: Task<Void, Never>?
    private var scopedWorkspacePairingGeneration: UInt64 = 0

    var hostPairingPayload: RemotePairingPayload? { hostPairingPresentation.payload }
    var hostPairingCode: String? { hostPairingPresentation.code }
    var hostPairingCompleted: Bool { hostPairingPresentation.completed }
    var remoteHostPairingPayload: RemotePairingPayload? {
        remoteHostPairingPresentation.payload
    }
    var remoteHostPairingCode: String? { remoteHostPairingPresentation.code }
    var remoteHostPairingCompleted: Bool { remoteHostPairingPresentation.completed }
    var scopedWorkspacePairingPayload: RemotePairingPayload? {
        scopedWorkspacePairingPresentation.payload
    }
    var scopedWorkspacePairingCode: String? { scopedWorkspacePairingPresentation.code }

    /// Merged (file + native overlay) presets for the editor.
    @Published private(set) var mergedPresets: [Preset] = []

    /// Legacy `setup_completed` from app-state.json. The onboarding wizard is
    /// gone (first run boots straight into the main UI with builtin presets
    /// seeded and every superpower on by default); the flag survives only to
    /// keep the legacy-preference migration and usage seeding one-shot for
    /// users who completed the old wizard.
    @Published private(set) var setupCompleted = false
    @Published private(set) var setupToolReport: ToolScanReport?
    /// True while a PATH scan is running — drives the Presets panel's
    /// Rescan button spinner.
    @Published private(set) var toolScanInProgress = false

    /// CLIs the Agent CLI Tools window's Install button is currently
    /// installing, and the last failure message per CLI (cleared on retry).
    @Published private(set) var toolInstallsInProgress: Set<SetupTool> = []
    @Published private(set) var toolInstallErrors: [SetupTool: String] = [:]

    /// Background AI-tool PATH scan (aiTools.ts isPresetAvailable parity).
    private let toolAvailability = ToolAvailability()

    /// Sessions indexed by id (flattened from nodes) for cheap lookup.
    private(set) var sessionsByID: [String: SessionEntry] = [:]

    /// Always-on safety-net rescan (5s). File events drive normal updates;
    /// this catches killed hosts (whose manifests never get a final write)
    /// and any missed/coalesced FSEvents.
    private var safetyTimer: Timer?
    /// External agents can add/remove Git worktrees without touching any
    /// Unpeel-owned file. Poll their cheap Git registry on the existing 5s
    /// safety cadence, not on every output-driven rescan.
    private var lastLinkedWorktreeDiscoveryAt = Date.distantPast
    private var linkedWorktreeDiscoveryInFlight = false
    /// 1s sweep that runs ONLY while some session is busy: it expires the
    /// 2.5s output-growth busy window and the 5-minute hook-busy deadline
    /// with the same timing the old always-on 1s timer had. No file event
    /// fires when output STOPS growing, so this cannot be event-driven.
    /// FSEvents stream over app-sessions + app-state.json
    /// (file-level events, 0.5s coalescing).
    private var fsEventStream: FSEventStreamRef?
    private var watchedPaths: [String] = []
    private var pendingRescanWork: DispatchWorkItem?
    private var pendingRescanDeadline: Date?
    /// Semantic content of the last activity-state.json write, so unchanged
    /// snapshots skip the disk write entirely.
    private var lastActivitySnapshotSignature: [String: String] = [:]
    /// Inputs of the last completed rescan, kept so overlay-only changes
    /// (drag-reorder) can rebuild the tree without re-hitting disk.
    private var lastScanProjects: [Project] = []
    private var lastScanSessions: [SessionEntry] = []
    private var lastScanTauriPins: [String: [PinnedSidebarSession]] = [:]
    private var hasCompletedScan = false
    private var initialScanTask: Task<Void, Never>?
    private var initialScanPending = false
    private let startupPresentationCache = StartupPresentationCache(home: LaunchConfig.unpeelDir)
    private var lastStartupPresentation: StartupPresentation?
    /// Final combined pin + regular order being previewed by an in-flight
    /// desktop session drag. This deliberately never reaches UserDefaults or
    /// session-order.json; a successful drop commits it once, while a
    /// cancelled drag removes it and rebuilds from durable state.
    private var sessionOrderPreviews: [String: [String]] = [:]
    /// Sibling order being previewed by an in-flight desktop project/worktree
    /// drag (one at a time), keyed by parent (nil = top-level). Same contract
    /// as `sessionOrderPreviews`: never persisted; a successful drop commits
    /// it once, a cancelled drag removes it and rebuilds from durable state.
    /// `draggedID` identifies the moved project so a remote-scope commit can
    /// send the one-project `project.organization.set` patch.
    private var projectOrderPreview: (parentID: String?, ids: [String], draggedID: String)?
    /// While any NSMenu is tracking (context menus, SwiftUI Menu dropdowns,
    /// the menu bar), rescans park here: a store publish mid-track makes
    /// SwiftUI rebuild the open menu's items, which visibly blinks flyout
    /// submenus. The UI shows a frozen snapshot for the few seconds a menu
    /// is open; the deferred rescan runs the moment tracking ends.
    private var menuTrackingDepth = 0
    private var rescanDeferredForMenuTracking = false
    private var scrollEventMonitor: Any?
    private var lastScrollWheelEventAt = Date.distantPast
    private var lastFullRescanAt = Date.distantPast
    /// Secondary local workspaces and remote Hosts are projected from a
    /// bootstrap snapshot. Applying one while AppKit is tracking a menu or a
    /// sidebar row owns an inline editor/confirm can replace that row out from
    /// under the interaction (the local rescan path is already menu-gated).
    /// Remember that a newer projection is waiting and apply the runtime's
    /// latest snapshot once the interaction releases its row.
    private var remoteProjectionDeferredForSidebarInteraction = false
    /// Per-project memo of the sidebar's rendered row lists; see
    /// `sidebarLists(in:)`. Cleared whenever an input changes.
    private var sidebarListsCache: [String: (pinned: [SessionEntry], displayed: [SessionEntry])] = [:]

    /// Last observed foreground-runtime identity (`id:pid`, "" while none),
    /// tracked only for sessions whose launch command is not hook-capable.
    /// Hook events carry no runtime identity, so this is what ties a hook
    /// latch to the observed process that produced it: a change to a new
    /// identity drops the previous latch. Absence of a key means "never
    /// scanned", which is deliberately not an edge — after an app restart the
    /// first sighting must keep a latch built from live events already
    /// accepted this launch.
    private var observedForegroundIdentities: [String: String] = [:]
    /// Last runtime launch generation observed per live Session. A generation
    /// edge invalidates hook activity from the preceding agent process while
    /// keeping the same Session and terminal identity.
    private var runtimeLaunchGenerations: [String: UInt64] = [:]
    /// Launch boundary for the latest observed in-place runtime generation.
    /// Hook receipt happens off-main, so this also rejects an old queued Stop
    /// after the transient restart-in-flight flag has cleared.
    private var runtimeLaunchCutoffs: [String: Date] = [:]
    /// Compatibility bound for hook assets installed by an older build. A
    /// legacy Stop is quarantined immediately after an in-place edge, but may
    /// settle after this window if the old provider never emits a recognized
    /// Start/UserPromptSubmit. Exact generation tags remain authoritative.
    private nonisolated static let legacyGenerationStopGuard: TimeInterval = 30
    /// Sessions launched by this app before their host writes manifest.json.
    /// These are UI-only rows; the manifest-backed entry replaces them as
    /// soon as rescan sees the real session on disk.
    private var pendingSessions: [String: SessionEntry] = [:]

    // MARK: Decode caches (mtime+size gated; skip JSON work when unchanged)

    /// (mtime, size) fingerprint of a file as of the last decode.
    fileprivate struct FileStamp: Equatable {
        var mtimeSec: Int
        var mtimeNsec: Int
        var size: Int64
    }

    /// Collector for the per-session-dir disk phase of a rescan (dir
    /// enumeration, manifest stat + cached decode, output.bin stats,
    /// archived markers). Owns the manifest decode cache so scheduled
    /// rescans can collect on a background queue — see `scheduleRescan`.
    let scanCollector = ScanSnapshotCollector()
    private let scanCollectQueue = DispatchQueue(
        label: "unpeel.scan-collect", qos: .userInitiated
    )
    private var scanCollectInFlight = false
    private var scanRecollectQueued = false
    /// Previous scan's derived entries plus the inputs they depended on,
    /// for exited-session reuse: ~90% of session dirs are exited and
    /// unchanged between rescans, and re-deriving them dominated the
    /// on-main apply phase (30-45ms per rescan, 1-2 rescans/s while
    /// anything streams).
    private var scanEntryCache: [String: SessionEntry] = [:]
    private var scanEntryCacheTitleOverrides: [String: String]?
    private var scanEntryCacheOverrideStamps: [String: FileStamp] = [:]
    private var scanEntryCacheArchivedDirs: Set<String> = []
    private var appStateCache: (stamp: FileStamp, file: AppStateFile?)?

    /// Single `stat(2)` call: much cheaper than FileManager.attributesOfItem
    /// (which builds a full NSDictionary incl. xattrs — it dominated the
    /// idle CPU profile at one rescan per second over ~57 session dirs).
    nonisolated fileprivate static func statFile(_ path: String) -> FileStamp? {
        var st = stat()
        guard stat(path, &st) == 0 else { return nil }
        return FileStamp(
            mtimeSec: Int(st.st_mtimespec.tv_sec),
            mtimeNsec: Int(st.st_mtimespec.tv_nsec),
            size: Int64(st.st_size)
        )
    }

    nonisolated private static func isNonemptyRegularFile(_ path: String) -> Bool {
        var st = stat()
        guard stat(path, &st) == 0 else { return false }
        return (st.st_mode & S_IFMT) == S_IFREG && st.st_size > 0
    }

    /// Evidence that this exact managed launch reached a provider-owned
    /// conversation. The Host may mint an id and create an empty managed
    /// directory before exec, so neither fact is enough on its own.
    nonisolated static func hasDurableResumeEvidence(
        manifest: HostedSessionManifest,
        dirPath: String
    ) -> Bool {
        let marker: [String: Any]? = {
            let markerPath = dirPath + "/" + SharedMarker.providerSession.rawValue
            guard let data = try? Data(contentsOf: URL(fileURLWithPath: markerPath))
            else { return nil }
            return (try? JSONSerialization.jsonObject(with: data)) as? [String: Any]
        }()
        let trimmed: (String?) -> String? = { value in
            guard let value = value?.trimmingCharacters(in: .whitespacesAndNewlines),
                  !value.isEmpty
            else { return nil }
            return value
        }
        let providerID = trimmed(marker?["provider_session_id"] as? String)
            ?? trimmed(manifest.providerSessionID)
        let transcriptPath = trimmed(marker?["provider_transcript_path"] as? String)
            ?? trimmed(manifest.providerTranscriptPath)
        // The provider's own non-empty transcript is durable proof that this
        // exact launch became a real resumable Session. It may appear before
        // (or outlive a staged replacement of) last-hook-event.json.
        if providerID != nil,
           let transcriptPath,
           isNonemptyRegularFile(transcriptPath) {
            return true
        }
        if providerID != nil,
           let data = try? Data(contentsOf: URL(
               fileURLWithPath: dirPath + "/last-hook-event.json"
           )),
           let event = (try? JSONSerialization.jsonObject(with: data)) as? [String: Any],
           let name = (event["hook_event_name"] ?? event["hookEventName"]) as? String,
           ["Start", "UserPromptSubmit", "Stop", "StopFailure", "PermissionRequest"]
            .contains(name) {
            return true
        }

        guard let managedPath = validatedManagedStoragePath(
            manifest.managedStoragePath,
            unpeelDir: LaunchConfig.unpeelDir
        ) else { return false }
        var pending = [URL(fileURLWithPath: managedPath, isDirectory: true)]
        var visited = 0
        while let directory = pending.popLast() {
            guard let children = try? FileManager.default.contentsOfDirectory(
                at: directory,
                includingPropertiesForKeys: [.isRegularFileKey, .isDirectoryKey, .isSymbolicLinkKey]
            ) else { continue }
            for child in children {
                visited += 1
                if visited > 256 { return false }
                guard let values = try? child.resourceValues(
                    forKeys: [.isRegularFileKey, .isDirectoryKey, .isSymbolicLinkKey]
                ), values.isSymbolicLink != true else { continue }
                if values.isRegularFile == true { return true }
                if values.isDirectory == true { pending.append(child) }
            }
        }
        return false
    }

    private static func unixMilliseconds(for stamp: FileStamp) -> UInt64? {
        guard stamp.mtimeSec >= 0, stamp.mtimeNsec >= 0 else { return nil }
        return UInt64(stamp.mtimeSec) * 1_000
            + UInt64(stamp.mtimeNsec) / 1_000_000
    }

    private static func jsonUInt64(_ value: Any?) -> UInt64? {
        guard let number = value as? NSNumber else { return nil }
        let signed = number.int64Value
        return signed >= 0 ? UInt64(signed) : nil
    }

    struct SharedTitleMarker: Equatable {
        let title: String?
        /// The writer's durable ordering timestamp. Markers from old builds
        /// fall back to their file mtime when decoded below.
        let updatedAt: UInt64?
    }

    /// title.json marker decode cache, same (mtime, size) gating — rescan
    /// consults the marker for every session, so the unchanged case must cost
    /// a stat, not a read + parse.
    private var titleMarkerCache: [
        String: (stamp: FileStamp, marker: SharedTitleMarker?)
    ] = [:]

    private func titleMarkerValue(
        sessionID: String,
        dirPath: String
    ) -> SharedTitleMarker? {
        let path = dirPath + "/" + SharedMarker.title.rawValue
        guard let stamp = Self.statFile(path) else {
            titleMarkerCache[sessionID] = nil
            return nil
        }
        if let cached = titleMarkerCache[sessionID], cached.stamp == stamp {
            return cached.marker
        }
        let object = FileManager.default.contents(atPath: path)
            .flatMap { (try? JSONSerialization.jsonObject(with: $0)) as? [String: Any] }
        let marker = object.map {
            SharedTitleMarker(
                title: $0["title"] as? String,
                updatedAt: Self.jsonUInt64($0["updated_at"])
                    ?? Self.unixMilliseconds(for: stamp)
            )
        }
        titleMarkerCache[sessionID] = (stamp, marker)
        return marker
    }

    /// project-override.json marker decode cache, same (mtime, size) gating
    /// as the title marker — rescan consults it for every session.
    private var projectOverrideCache: [String: (stamp: FileStamp, id: String?)] = [:]

    private func projectOverrideValue(sessionID: String, dirPath: String) -> String? {
        let path = dirPath + "/" + SharedMarker.projectOverride.rawValue
        guard let stamp = Self.statFile(path) else {
            projectOverrideCache[sessionID] = nil
            return nil
        }
        if let cached = projectOverrideCache[sessionID], cached.stamp == stamp {
            return cached.id
        }
        let id = FileManager.default.contents(atPath: path)
            .flatMap { (try? JSONSerialization.jsonObject(with: $0)) as? [String: Any] }
            .flatMap { $0["project_id"] as? String }
        projectOverrideCache[sessionID] = (stamp, id)
        return id
    }

    /// Re-seed the in-memory hook latch from the last lifecycle event the
    /// session's hook scripts persisted to disk (last-hook-event.json). Hook
    /// scripts keep firing while no app instance is listening, so after an app
    /// restart this restores busy/attention for sessions that were mid-turn —
    /// and correctly stays idle when the turn finished while the app was
    /// closed.
    private func seedHookActivity(
        sessionID: String,
        dirPath: String,
        runtimeGeneration: UInt64,
        runtimeLaunchedAt: Date?,
        anchorStartEventToOutput: Bool = true
    ) {
        let path = dirPath + "/last-hook-event.json"
        guard let stamp = Self.statFile(path),
              let data = FileManager.default.contents(atPath: path),
              let event = LastHookEvent.parse(data)
        else { return }
        let receivedAt = Self.stampDate(stamp)
        let decision = Self.hookRuntimeDecision(
            eventGeneration: event.runtimeGeneration,
            hookEventName: event.hookEventName,
            receivedAt: receivedAt,
            currentGeneration: runtimeGeneration,
            runtimeLaunchedAt: runtimeLaunchedAt,
            currentGenerationOwned: activity.hasRuntimeOwnership(
                sessionID, generation: runtimeGeneration
            )
        )
        guard case let .accept(effectiveGeneration) = decision else { return }
        var seedAt = receivedAt
        // Turns routinely run longer than the 5-minute hook idle timeout, so
        // a busy seed anchored at the event's own mtime would expire on the
        // first sweep for any long turn. While the agent works, the hosted
        // PTY keeps appending streamed output to output.bin — for an open
        // turn (Start/UserPromptSubmit with no Stop recorded after it), a
        // fresh output.bin means "still working right now", so anchor the
        // deadline at whichever timestamp is fresher. A recorded Stop always
        // wins: idle sessions stay idle no matter how the TUI repaints, and
        // a stale open turn (agent died mid-turn) still expires on the first
        // sweep because both timestamps are old.
        if event.shouldAnchorSeedToOutput(anchorStartEventToOutput: anchorStartEventToOutput),
           let outputStamp = Self.statFile(dirPath + "/output.bin") {
            seedAt = max(seedAt, Self.stampDate(outputStamp))
        }
        activity.applyHookEvent(
            sessionID: sessionID,
            hookEventName: event.hookEventName,
            latchOnly: event.latchOnly,
            runtimeGeneration: effectiveGeneration,
            now: seedAt
        )
    }

    private static func stampDate(_ stamp: FileStamp) -> Date {
        Date(timeIntervalSince1970:
            TimeInterval(stamp.mtimeSec) + TimeInterval(stamp.mtimeNsec) / 1_000_000_000)
    }

    /// Maintenance compatibility floor. Bounded journaling is adopted when a
    /// Host is naturally replaced; a healthy v2/v3 Host must not recommend a
    /// disruptive reload merely to reclaim its existing terminal journal.
    private static let requiredSessionHostProtocolVersion = 2

    // MARK: Hook-driven activity (session_activity.rs / sessionState.ts)

    /// Hook latch + busy/idle/attention per session. Output can re-arm an
    /// authoritative hook-owned turn, but never creates one.
    private let activity = SessionActivityEngine()

    /// Hook server owned by the app delegate; attached after init so the
    /// preset self-test's throwaway `UnpeelStore()` never starts one.
    private(set) var hookServer: HookServer?

    /// Sessions whose last hook event was Stop (completedSessionIds in
    /// sessionState.ts) — feeds the pending-unread reconciliation.
    private var completedSessionIDs: Set<String> = []


    /// Busy/attention sessions the user switched away from; they become
    /// unread when they settle (sessionUnread.ts pendingUnreadSessions).
    private var pendingUnreadSessions: Set<String> = []

    /// Sessions whose current `menu_prompt_active` flag the user dismissed
    /// ("Clear attention" in the sidebar context menu). The host's flag is
    /// level-held while the detected menu stays on screen, so a plain clear
    /// would re-badge on the next rescan; this set suppresses the override
    /// until the host lowers the flag, which re-arms detection for the next
    /// real menu. In-memory only — a stale false positive shouldn't survive
    /// an app relaunch as a dismissal.
    private var menuAttentionDismissals: Set<String> = []
    /// Raw host menu state, kept separately from the derived attention badge so
    /// only a false -> true edge emits a notification. The runtime generation
    /// is part of the identity: an in-place agent restart resets the host flag,
    /// and a fast new prompt must still re-arm even if this app missed the
    /// intermediate false manifest write.
    private var menuPromptNotificationStates: [String: MenuPromptNotificationState] = [:]
    private var previousObservedSessionID: String?
    private var appActivationObservers: [NSObjectProtocol] = []

    /// The picker scopes the whole workspace. Local state remains loaded so
    /// switching back is instant, but every remote surface/verb must use the
    /// remote backend and the spawn boundary below refuses local execution.
    @Published private(set) var selectedHostScope: SelectedHostScope = .local
    /// Display name of the selected non-local scope, nil for Local. Feeds
    /// the interim Settings scope labeling (Settings edits THIS instance's
    /// workspace regardless of the picker — see
    /// the interim scope labeling in SettingsView).
    var selectedScopeDisplayName: String? {
        switch selectedHostScope {
        case .local:
            return nil
        case let .localWorkspace(_, name):
            return name
        case let .remote(hostID):
            return remoteHostStore.records.first { $0.hostID == hostID }?.name
                ?? "the selected Host"
        }
    }
    /// Settings scope selector (scope rule shared with
    /// SettingsView): the dropdown FOLLOWS the window's active workspace by
    /// default — while a non-local scope is selected, the workspace-scoped
    /// Settings panels target it over its existing connection. Picking
    /// "This Mac" in the dropdown pins Settings to this instance's own
    /// workspace; any scope change re-follows.
    @Published var settingsScopePinnedToThisMac = false
    var settingsScopeTargetsHost: Bool {
        !settingsScopePinnedToThisMac && selectedHostScope != .local
    }
    let localHostID: String
    let remoteHostStore: RemoteHostStore
    let remoteHostRuntime = RemoteHostRuntime()
    /// Background workspace pool (workspaces-unification phase 7): read-only
    /// bootstrap connections + cached snapshots for every known workspace the
    /// runtime is NOT serving. Started ~2s after the first window paint
    /// behind the picker gate (`startWorkspacePoolAfterFirstPaint`).
    let workspacePool = WorkspacePool()
    /// Shared, incrementally cached activity projection for every workspace.
    lazy var globalActivityMenu = GlobalActivityMenuModel(
        store: self, pool: workspacePool
    )
    /// One-shot latch for the deferred pool spin-up above.
    private var workspacePoolStartScheduled = false
    /// The sidebar's live pager state, registered by SidebarView so footer
    /// dot / selector / settings switches can run the single-slide page
    /// transition. Weak: the pager lives and dies with the sidebar.
    weak var workspacePagerAnimator: SidebarWorkspacePager?
    private var localSelectedSessionIDBeforeRemote: String?
    /// Last selected Session per non-local scope (pane scope key), for this
    /// app run: returning to a workspace/Host re-selects where you left off
    /// instead of the top row. Deliberately in-memory only, like the Local
    /// twin above.
    private var scopeSessionMemory: [String: String] = [:]

    /// The remembered last selection for a scope — the swipe preview uses
    /// this so the pooled page highlights the row the committed scope will
    /// restore, instead of flashing the Host's default (top) row.
    func rememberedScopeSelection(paneScopeID: String) -> String? {
        scopeSessionMemory[paneScopeID]
    }

    /// Capture the outgoing scope's selection before a scope switch clears
    /// it. Call at the top of every scope-changing verb.
    private func rememberOutgoingScopeSelection() {
        guard selectedHostScope != .local, let id = selectedSessionID else { return }
        scopeSessionMemory[selectedHostScope.paneScopeID] = id
    }

    // MARK: Remote-scope display projection
    //
    // Local truth (`nodes`, `sessionsByID`, `projectsByID`, presets, pins,
    // unread/archived sets) always tracks THIS Mac — it also feeds the
    // /mobile Host serving path, which keeps serving paired phones while a
    // remote Host is selected. Remote scope projects the selected Host's
    // bootstrap into these parallel structures, and the views read the
    // `display*` accessors so the SAME sidebar/content hierarchy renders
    // either source. Nothing here is ever persisted locally.
    @Published private(set) var remoteNodes: [ProjectNode] = []
    @Published private(set) var remoteSessionsByID: [String: SessionEntry] = [:]
    @Published private(set) var remoteProjectsByID: [String: Project] = [:]
    @Published private(set) var remoteArchivedByProject: [String: [SessionEntry]] = [:]
    private(set) var remoteSummariesByID: [String: RemoteSessionSummary] = [:]
    private var remoteArchivedSummaryCache = RemoteArchivedSessionSummaryCache()
    private var remoteArchivePageGeneration: UInt64 = 0
    private var remoteProjectSummariesByID: [String: RemoteProjectSummary] = [:]
    private var remoteSessionOrderByProject: [String: [String]] = [:]
    private var remotePresetSummaries: [RemotePresetSummary] = []
    private var remotePresets: [Preset] = []
    private var remoteQuickPresetGroups: [QuickPresetGroup] = []
    /// Host key whose root projects were auto-expanded once per selection, so
    /// entering a remote Host never lands on an all-collapsed tree.
    private var remoteAutoExpandedHostKey: String?
    /// Session id whose project chain was last auto-revealed by the remote
    /// projection. Reveal must fire only when the selection changes — the
    /// projection also re-runs on every drag-preview hover, and re-expanding
    /// a deliberately collapsed project mid-drag pops it open under the cursor.
    private var remoteRevealedSelectionID: String?
    /// Optimistic sibling order held after a remote reorder commit until a
    /// bootstrap confirms it, the verb fails, or the hold expires. Without
    /// it, a periodic bootstrap captured BEFORE the Host applied the write
    /// can land right after the drop and visibly snap the drag back.
    private var remoteCommittedOrderHold:
        (parentID: String?, ids: [String], heldAt: Date)?
    /// Session-list counterpart to `remoteCommittedOrderHold`. The live
    /// projection and the carousel pool both retain the dropped order until
    /// a bootstrap confirms it, so neither surface flashes stale ranks.
    private struct RemoteCommittedSessionOrderHold {
        let workspaceKey: String?
        let ids: [String]
        let heldAt: Date
    }
    private var remoteCommittedSessionOrderHolds:
        [String: RemoteCommittedSessionOrderHold] = [:]
    private var remoteScopeCancellables: Set<AnyCancellable> = []
    /// The Local migration never swaps the sidebar to Host-backed structures
    /// until one complete snapshot has been projected. Disk-rescan state stays
    /// visible during service startup/recovery, avoiding an empty-state flash.
    @Published private(set) var localHostProjectionReady = false
    private var localHostClientStarted = false
    /// Platform-adapter callbacks from the worker (`overlay.project-color.set`)
    /// must apply their native effect locally and return an honest
    /// synchronous receipt instead of routing back through the Host verb.
    /// App UI actions run at depth zero and route through `host.sock`.
    private var nativeHostAdapterEffectDepth = 0

    static let sidebarCollapsedKey = "unpeel.sidebar.collapsed"
    /// Last Local main-area Session shown in the content pane. Restored
    /// after the first rescan so a relaunch does not land on the empty
    /// state. Project-sidebar members are skipped: selecting one leaves
    /// the main pane empty on purpose, and must not overwrite the last
    /// real content-pane Session.
    static let selectedSessionKey = "unpeel.native.selectedSession"
    static let expandedProjectsKey = "unpeel.native.expandedProjects"
    static let menuAttentionDetectionKey = "unpeel.native.menuAttentionDetection"
    static let showSessionGalleryKey = "unpeel.native.showSessionGallery"
    /// Historical key spelling: the setting used to live on Settings ▸
    /// Workspaces as "Show agent workspaces". The UI is now Settings ▸
    /// Worktrees; the persisted key is load-bearing.
    static let showAgentWorkspacesKey = "unpeel.native.showAgentWorkspaces"
    static let commandTActionKey = "unpeel.native.commandTAction"
    private static let nativePinsKey = "unpeel.sidebar.pins"
    private static let nativePendingTitleWritesKey = "unpeel.native.pendingTitleWrites"
    static let nativePresetsKey = "unpeel.native.presets"
    static let nativeThemeKey = "unpeel.native.theme"
    nonisolated static let nativeAppTintKey = "unpeel.native.appTint"
    private nonisolated static let nativeCodeEditorKey = "unpeel.native.codeEditor"
    private static let appPresentationReceiptsKey =
        "unpeel.native.appPresentationReceipts.v1"
    // Legacy cleanup keys (pre auto-stop-and-archive merge). The stop-minutes
    // value is folded once into app-state.json's `auto_stop_archive_minutes`
    // (the shared truth the TUI also reads); the cleanup-days setting was
    // removed outright.
    private static let legacyAutoSessionCleanupDaysKey = "unpeel.native.autoSessionCleanupDays"
    private static let legacyAutoSessionStopMinutesKey = "unpeel.native.autoSessionStopMinutes"
    static let nativePresetOrderKey = "unpeel.native.presetOrder"
    // Legacy per-CLI preference keys (pre flat-preset-list). Read once by
    // `migrateCLIPreferencesIfNeeded`; left in place afterwards so older
    // builds sharing the defaults suite keep working.
    private static let legacyCLIAvailabilityKey = "unpeel.native.cliAvailability"
    private static let legacyCLIDefaultsKey = "unpeel.native.cliDefaults"
    private static let legacyCLIOrderKey = "unpeel.native.cliOrder"
    private static let nativeProjectFolderColorsKey = "unpeel.native.projectFolderColors"

    static let autoStopArchiveMinuteOptions = [0, 30, 60, 120, 240, 480, 1440]

    /// Opt-out default: a day of unbroken idleness before the terminal is
    /// stopped and archived. Safe because archive is non-destructive
    /// (Restore + Restart resumes the conversation) and plain shells are
    /// exempt entirely.
    static let defaultAutoStopArchiveMinutes = 1440

    static func autoStopArchiveLabel(for minutes: Int) -> String {
        switch minutes {
        case 0: return "Never"
        case ..<60: return "After \(minutes) minutes"
        case 60: return "After 1 hour"
        case 1440: return "After 1 day"
        default: return "After \(minutes / 60) hours"
        }
    }

    init(
        paneLayoutController: PaneLayoutController = PaneLayoutController(),
        deferInitialScan: Bool = false
    ) {
        self.paneLayoutController = paneLayoutController
        localHostID = MobilePairingStore.defaultMacID()
        remoteHostStore = RemoteHostStore(localHostID: localHostID)
        sidebarCollapsed = AppDefaults.shared.bool(forKey: Self.sidebarCollapsedKey)
        menuAttentionDetectionEnabled = Self.resolveMenuAttentionDetection()
        showSessionGallery = AppDefaults.shared.bool(forKey: Self.showSessionGalleryKey)
        showAgentWorktrees = AppDefaults.shared.bool(forKey: Self.showAgentWorkspacesKey)
        commandTAction = CommandTAction(
            rawValue: AppDefaults.shared.string(forKey: Self.commandTActionKey) ?? ""
        ) ?? .newTerminal
        projectFolderColorIDs = Self.loadProjectFolderColorIDs()
        Self.migrateAutoStopArchiveSetting()
        activityLogEntries = activityLog.entries
        presetOrder = Self.loadPresetOrder()
        paneLayoutController.objectWillChange
            .sink { [weak self] _ in self?.objectWillChange.send() }
            .store(in: &remoteScopeCancellables)
        // Mirror the selection's root project into published store state (see
        // `activeRootProjectID`) — async because $sessionID emits on willSet.
        sessionSelection.$sessionID
            .receive(on: DispatchQueue.main)
            .sink { [weak self] id in self?.refreshActiveRootProject(selectedID: id) }
            .store(in: &remoteScopeCancellables)
        // Publish-rate probe (UNPEEL_DEBUG=1 only): every objectWillChange
        // re-evaluates every store-observing SwiftUI view — the whole
        // sidebar. A high steady rate here IS the frame-budget thief;
        // profiles show the SwiftUI graph churn but not who published.
        if ProcessInfo.processInfo.environment["UNPEEL_DEBUG"] == "1" {
            var publishCount = 0
            var publishWindow = Date()
            objectWillChange
                .sink { _ in
                    publishCount += 1
                    let now = Date()
                    if now.timeIntervalSince(publishWindow) >= 1 {
                        NSLog("[perf] store objectWillChange %d/s", publishCount)
                        publishCount = 0
                        publishWindow = now
                    }
                }
                .store(in: &remoteScopeCancellables)
        }
        refreshToolAvailability()
        migrateAwayFromPerSessionMediaAccess()
        if deferInitialScan {
            applyAppAppearance()
            restoreStartupPresentation()
            beginInitialScan()
        } else {
            rescan()
            repairDuplicatePresetsOnce()
            restorePersistedSessionSelection()
        }
        Self.compactStoppedOutputJournals()
        // Host-scope projection inputs. Remote/scoped workspaces always use
        // them; the Local migration uses them only after its explicit Dev-
        // gated client has started and a full snapshot is ready.
        // Delivered on the next main-loop tick (never at willSet time), so
        // the runtime's own state is settled when the projection reads it.
        remoteHostRuntime.$snapshot
            .receive(on: DispatchQueue.main)
            .sink { [weak self] snapshot in
                guard let self,
                      self.selectedHostScope != .local
                        || self.localHostClientStarted
                else { return }
                self.projectRemoteScope(snapshot: snapshot)
                // A remote Host's advertised tint (hostTintHue) arrives with
                // its bootstrap, after the scope switch — repaint when it does.
                self.applyScopeTint()
                self.refreshScopeAppearance()
            }
            .store(in: &remoteScopeCancellables)
        remoteHostRuntime.$selectedSessionID
            .receive(on: DispatchQueue.main)
            .sink { [weak self] sessionID in
                guard let self else { return }
                if self.selectedHostScope == .local {
                    // Local launcher/focus is Controller-window state. Mirrored
                    // runtime values are deliberately one-way (store → runtime)
                    // so an older queued value cannot close a just-opened New
                    // Terminal launcher or bounce selection to the prior row.
                    return
                }
                if self.selectedSessionID != sessionID {
                    self.selectedSessionID = sessionID
                }
            }
            .store(in: &remoteScopeCancellables)
        remoteHostRuntime.$directDataPlaneSelectionIntent
            .receive(on: DispatchQueue.main)
            .sink { [weak self] intent in
                guard let self, let intent,
                      self.selectedHostScope == .local,
                      self.localHostClientStarted,
                      self.localHostProjectionReady
                else { return }
                if let sessionID = intent.sessionID,
                   self.remoteHostRuntime.snapshot?.sessions.contains(where: {
                       $0.id == sessionID
                   }) != true {
                    return
                }
                if self.selectedSessionID != intent.sessionID {
                    self.selectedSessionID = intent.sessionID
                }
            }
            .store(in: &remoteScopeCancellables)
        // Connection-state changes repaint the host button and banners.
        remoteHostRuntime.$connectionState
            .removeDuplicates()
            .receive(on: DispatchQueue.main)
            .sink { [weak self] state in
                guard let self,
                      self.selectedHostScope != .local
                        || self.localHostClientStarted
                else { return }
                self.objectWillChange.send()
                guard self.selectedHostScope == .local,
                      self.localHostClientStarted
                else { return }
                switch state {
                case .reconnecting(let message), .failed(let message):
                    HostServiceManager.shared.noteLocalConnectionFailed(reason: message)
                case .connected:
                    HostServiceManager.shared.noteLocalConnectionEstablished()
                case .idle, .connecting, .repairRequired, .incompatible:
                    break
                }
            }
            .store(in: &remoteScopeCancellables)
        // Workspace pool (phase 7): mirror the runtime's snapshots into the
        // pool cache — the runtime-served workspace (foreground or warm) is
        // excluded from pooling, so this mirror is what keeps its peek
        // content and attention detection current with one live connection.
        remoteHostRuntime.$snapshot
            .receive(on: DispatchQueue.main)
            .sink { [weak self] snapshot in
                guard let self, let snapshot,
                      let (key, name) = self.runtimeServedWorkspaceKeyAndName()
                else { return }
                self.workspacePool.noteExternalSnapshot(
                    snapshot, forKey: key, name: name
                )
            }
            .store(in: &remoteScopeCancellables)
        // The pool itself starts later — ~2s after the first window paint
        // (`startWorkspacePoolAfterFirstPaint`, called from RootView) — so
        // its reconcile and gateway children never compete with first-frame
        // work. Everything that touches it before then no-ops behind the
        // pool's own `started` guard.
        if RemoteHostFeature.pickerEnabled,
           let hostID = remoteHostStore.selectedHostID {
            localSelectedSessionIDBeforeRemote = selectedSessionID
            selectedSessionID = nil
            selectedHostScope = .remote(hostID: hostID)
            settingsScopePinnedToThisMac = false
            paneLayoutController.switchScope(to: selectedHostScope.paneScopeID)
            if let host = remoteHostStore.records.first(where: { $0.hostID == hostID }),
               let credentials = remoteHostStore.credentials(for: hostID) {
                connectRemoteHost(host, credentials: credentials)
            } else if let host = remoteHostStore.sshRecords.first(where: { $0.id == hostID }) {
                connectSSHHost(host)
            }
            projectRemoteScope(snapshot: remoteHostRuntime.snapshot)
        }
        // Rescans are normally event-driven (FSEvents on app-sessions and
        // the preset files, set up by rescan() above). The 5s timer is a
        // safety net for killed hosts and missed events; a separate 1s sweep
        // runs only while some session is busy (see updateBusySweepTimer).
        let timer = Timer(timeInterval: 5.0, repeats: true) { [weak self] _ in
            // Off-main collection: the safety net fires forever, and its
            // disk pass must never cost the main queue a frame.
            Task { @MainActor in self?.scheduleRescan(after: 0) }
        }
        RunLoop.main.add(timer, forMode: .common)
        safetyTimer = timer

        // Window focus gates the "observed" session (sessionUnread.ts
        // getObservedWorkspaceSessionId: documentVisible && windowFocused).
        for name in [NSApplication.didBecomeActiveNotification,
                     NSApplication.didResignActiveNotification] {
            let becameActive = name == NSApplication.didBecomeActiveNotification
            appActivationObservers.append(
                NotificationCenter.default.addObserver(
                    forName: name, object: nil, queue: .main
                ) { [weak self] _ in
                    Task { @MainActor in
                        self?.handleObservationChanged()
                        // Coming to the foreground is one of the pool's
                        // immediate-refresh triggers (throttled inside).
                        if becameActive {
                            self?.workspacePool.requestImmediateRefresh()
                        }
                    }
                }
            )
        }

        // Menu tracking gates rescans (see menuTrackingDepth). Depth-counted:
        // AppKit can post begin/end per menu in a tracking session, and the
        // sweep/safety timers run in .common mode so they DO fire mid-track.
        for (name, delta) in [(NSMenu.didBeginTrackingNotification, 1),
                              (NSMenu.didEndTrackingNotification, -1)] {
            appActivationObservers.append(
                NotificationCenter.default.addObserver(
                    forName: name, object: nil, queue: .main
                ) { [weak self] _ in
                    MainActor.assumeIsolated { self?.applyMenuTracking(delta) }
                }
            )
        }

        // Scroll tracking gates rescans the same way menus do (see the
        // defer in `rescan()`). ghostty presents every terminal frame via a
        // main-queue hop, so a rescan burst mid-scroll reads as an FPS drop.
        // The monitor itself only stamps a date — no per-event work.
        scrollEventMonitor = NSEvent.addLocalMonitorForEvents(
            matching: .scrollWheel
        ) { [weak self] event in
            MainActor.assumeIsolated { self?.lastScrollWheelEventAt = Date() }
            return event
        }
    }

    /// Upgrade maintenance: old builds retained every terminal repaint
    /// forever. Reclaim only exited journals in the background; live pre-v4
    /// Hosts remain untouched and surface the normal Reload Terminal action.
    private nonisolated static func compactStoppedOutputJournals() {
        DispatchQueue.global(qos: .utility).async {
            let process = Process()
            process.executableURL = URL(fileURLWithPath: LaunchConfig.hostBinary)
            process.arguments = ["__compact_output_journals__"]
            process.standardInput = FileHandle.nullDevice
            process.standardOutput = FileHandle.nullDevice
            process.standardError = FileHandle.nullDevice
            guard (try? process.run()) != nil else { return }
            process.waitUntilExit()
        }
    }

    /// The app can paint its saved rows while manifest collection runs. Local
    /// clients wait for this initial fallback so it cannot replace Host truth.
    func waitForInitialScan() async {
        await initialScanTask?.value
    }

    private func beginInitialScan() {
        initialScanPending = true
        let collector = scanCollector
        let root = LaunchConfig.appSessionsDir.path
        let ttl = Self.purgedSessionDirTTL
        initialScanTask = Task { @MainActor [weak self] in
            let snapshot = await Task.detached(priority: .userInitiated) {
                collector.collect(root: root, purged: [:], purgedTTL: ttl, now: Date())
            }.value
            guard let self else { return }
            self.initialScanPending = false
            self.performRescan(snapshot: snapshot)
            self.repairDuplicatePresetsOnce()
            self.restorePersistedSessionSelection()
            self.refreshInstalledApps()
            self.initialScanTask = nil
        }
    }

    private func restoreStartupPresentation() {
        guard let saved = startupPresentationCache.load(
            home: Self.currentInstanceNormalizedHome(), hostID: localHostID
        ) else { return }
        lastStartupPresentation = saved
        nodes = saved.nodes
        var projects: [String: Project] = [:]
        var sessions: [String: SessionEntry] = [:]
        func collect(_ nodes: [ProjectNode]) {
            for node in nodes {
                projects[node.id] = node.project
                for session in node.sessions { sessions[session.id] = session }
                collect(node.worktrees)
            }
        }
        collect(saved.nodes)
        projectsByID = projects
        sessionsByID = sessions
        pinnedByProject = saved.pins
        archivedSessionIDs = saved.archivedIDs
        unreadSessionIDs = saved.unreadIDs
        restorePersistedSessionSelection()
        // hasCompletedScan deliberately stays false: cached status is only
        // presentation, never input to lifecycle edge/notification detection.
    }

    private func saveStartupPresentation(
        nodes: [ProjectNode], summaries: [RemoteSessionSummary]
    ) {
        var pins: [String: [PinnedSidebarSession]] = [:]
        for session in summaries where session.pinned {
            pins[session.projectID, default: []].append(PinnedSidebarSession(
                key: PinnedSidebarSession.key(forSessionID: session.id),
                projectID: session.projectID, sessionID: session.id, pinnedAt: 1
            ))
        }
        let value = StartupPresentation(
            home: Self.currentInstanceNormalizedHome(), hostID: localHostID,
            nodes: nodes, pins: pins,
            archivedIDs: Set(summaries.filter(\.archived).map(\.id)),
            unreadIDs: Set(summaries.filter(\.unread).map(\.id))
        )
        guard value != lastStartupPresentation else { return }
        lastStartupPresentation = value
        startupPresentationCache.save(value)
    }

    func startLocalHostClient() {
        localHostClientStarted = true
        // The initial disk scan remains as a no-flash fallback until the
        // first complete Host bootstrap. From this point forward the worker
        // is the sole lifecycle authority.
        // Old native Remove actions were stored as Controller-local tombstones.
        // Fold them into shared Host truth before the first projection so the
        // service cannot resurrect an intentionally removed legacy project.
        mirrorProjectsToSharedState()
        connectLocalHostServiceIfNeeded()
    }

    private func connectLocalHostServiceIfNeeded() {
        guard localHostClientStarted,
              selectedHostScope == .local
        else { return }
        let home = Self.currentInstanceNormalizedHome()
        if remoteHostRuntime.warmConnectionMatches(
            pinnedHostID: localHostID,
            workspaceHome: home
        ) {
            projectRemoteScope(snapshot: remoteHostRuntime.snapshot)
            return
        }
        localHostProjectionReady = false
        let localKey = WorkspaceListOrder.localKey(home: home)
        remoteHostRuntime.connectLocalService(
            home: home,
            name: workspaceDisplayName(forKey: localKey),
            expectedHostID: localHostID
        )
        // Returning from another workspace: while this runtime slot served
        // Local, its snapshots were mirrored into the pool. Seed the fresh
        // connection with that last Host truth and project it in this same
        // transaction — exactly the pooled-seed handoff every other scope
        // gets. Without it the tree falls back to the Swift scan, which is
        // frozen at launch once Local is a Host client: every Session
        // created since (most visibly the ones filed inside groups) was
        // missing until the first bootstrap landed, so they popped in late.
        if let seed = workspacePool.snapshot(forKey: localKey) {
            remoteHostRuntime.seedSnapshot(seed)
            projectRemoteScope(snapshot: remoteHostRuntime.snapshot)
        }
    }

    /// The only Host-scope mutation path. Remote connection failures never
    /// call this, so an offline Host cannot silently fall back to Local and
    /// turn the next user action into a local effect.
    func selectHost(_ hostID: String?, forceReconnect: Bool = false) {
        rememberOutgoingScopeSelection()
        if let hostID {
            guard RemoteHostFeature.pickerEnabled else { return }
            if selectedHostScope.remoteHostID == hostID,
               remoteHostRuntime.selectionConnectionIsActive,
               !forceReconnect {
                // A checked Host is already the current scope. Reopening its
                // backend could duplicate an in-flight semantic effect from a
                // candidate bootstrap captured before the old tail settles.
                return
            }
            cancelRemoteHostPairing()
            let pairedHost = remoteHostStore.records.first(where: { $0.hostID == hostID })
            let pairedCredentials = pairedHost.flatMap {
                remoteHostStore.credentials(for: $0.hostID)
            }
            let sshHost = remoteHostStore.sshRecords.first(where: { $0.id == hostID })
            guard (pairedHost != nil && pairedCredentials != nil) || sshHost != nil else {
                return
            }
            localHostProjectionReady = false
            if selectedHostScope == .local {
                localSelectedSessionIDBeforeRemote = selectedSessionID
            }
            selectedSessionID = nil
            // Settings stays open across a scope switch: rescoping from the
            // Workspaces screen should just retint/retitle in place (the
            // sidebar-selector case has settings closed anyway).
            commandPaletteVisible = false
            cancelSessionSwitcher()
            commandHintsVisible = false
            setControlHintsVisible(false)
            launcherProjectID = nil
            archivedProjectID = nil
            recentActivityVisible = false
            remoteHostStore.selectHost(hostID)
            selectedHostScope = .remote(hostID: hostID)
            settingsScopePinnedToThisMac = false
            paneLayoutController.switchScope(to: selectedHostScope.paneScopeID)
            // Returning to a Host whose background connection is still warm
            // (kept alive across the trip to Local) adopts it as-is: no SSH/
            // bootstrap churn, and the retained snapshot + pane cache render
            // immediately. Reuse of a LIVE connection is safe where a reopen
            // is not — nothing is retired, so no in-flight effect can be
            // duplicated. Anything less than an active identity match takes
            // the normal connect path.
            let warmReusable = !forceReconnect
                && remoteHostRuntime.warmConnectionMatches(
                    pinnedHostID: pairedHost != nil ? hostID : sshHost?.hostID
                )
            // Workspace-pool handoff: retire the pool's read-only connection
            // for this Host (the runtime becomes its only live connection)
            // and take its cached snapshot so the switch renders real rows
            // immediately instead of a skeleton.
            let poolKey = pairedHost != nil
                ? WorkspaceListOrder.pairedKey(hostID: hostID)
                : WorkspaceListOrder.sshKey(id: hostID)
            let pooledSeed = workspacePool.lendConnection(forKey: poolKey)
            if !warmReusable {
                if let pairedHost, let pairedCredentials {
                    connectRemoteHost(pairedHost, credentials: pairedCredentials)
                } else if let sshHost {
                    connectSSHHost(sshHost)
                }
                if let pooledSeed {
                    remoteHostRuntime.seedSnapshot(pooledSeed)
                }
            }
            projectRemoteScope(snapshot: remoteHostRuntime.snapshot)
            applyScopeTint()
            refreshScopeAppearance()
            workspacePool.refreshTargets()
            return
        }

        cancelRemoteHostPairing()
        // Before the Local-as-client migration the remote connection stayed
        // warm here. In Dev, Local now takes this same runtime slot and the
        // workspace pool resumes background reads for the Host we just left.
        // Release retains the warm-remote behavior until parity gates flip.
        remoteHostStore.selectHost(nil)
        selectedHostScope = .local
        // Nothing non-local to target; the next scope change re-follows.
        settingsScopePinnedToThisMac = false
        paneLayoutController.switchScope(to: selectedHostScope.paneScopeID)
        clearRemoteScopeProjection()
        // The remote selection belongs to the scope we just left. Never
        // carry it into Local, where an id collision could select an
        // unrelated Session or pane group.
        selectedSessionID = nil
        // Reconnect (and, as a Host client, re-project the mirrored Local
        // snapshot) BEFORE restoring the prior selection: the restore must
        // check Host truth, not the launch-frozen Swift scan, or a Session
        // created since launch would lose its selection on every return.
        connectLocalHostServiceIfNeeded()
        if let prior = localSelectedSessionIDBeforeRemote,
           displaySessionsByID[prior] != nil {
            selectedSessionID = prior
        }
        localSelectedSessionIDBeforeRemote = nil
        refreshTitlebarBranch()
        applyScopeTint()
        refreshScopeAppearance()
        // The just-left Host stays runtime-served (warm) and thus excluded;
        // reconcile so any workspace the runtime dropped resumes pooling.
        workspacePool.refreshTargets()
    }

    /// Scope this window to another LOCAL workspace over the loopback gateway
    /// (workspaces-unification phase 2). Same rules as a remote Host scope:
    /// verbs ride the Host connection, local spawns stay refused, and coming
    /// back is `selectHost(nil)`. Selection is deliberately not persisted —
    /// a relaunch starts in this instance's own Local scope.
    func selectLocalWorkspace(_ record: UnpeelWorkspaceRecord) {
        selectLocalWorkspace(home: record.home, name: record.name)
    }

    func selectLocalWorkspace(home: String, name: String) {
        rememberOutgoingScopeSelection()
        let normalizedHome = UnpeelWorkspaceRegistry.normalizePath(home)
        if selectedHostScope.localWorkspaceHome == normalizedHome,
           remoteHostRuntime.selectionConnectionIsActive {
            // Reopening the checked workspace's backend could duplicate an
            // in-flight semantic effect, exactly like a re-selected Host.
            return
        }
        cancelRemoteHostPairing()
        localHostProjectionReady = false
        if selectedHostScope == .local {
            localSelectedSessionIDBeforeRemote = selectedSessionID
        }
        selectedSessionID = nil
        // Settings stays open across a scope switch (see selectHost).
        commandPaletteVisible = false
        cancelSessionSwitcher()
        commandHintsVisible = false
        setControlHintsVisible(false)
        launcherProjectID = nil
        archivedProjectID = nil
        recentActivityVisible = false
        remoteHostStore.selectHost(nil)
        selectedHostScope = .localWorkspace(home: normalizedHome, name: name)
        settingsScopePinnedToThisMac = false
        // A never-started workspace has no home dir yet; create it exactly
        // like UnpeelWorkspaceRegistry.create does so the gateway and a later
        // app instance agree on one state dir.
        let homeURL = URL(fileURLWithPath: normalizedHome, isDirectory: true)
        try? FileManager.default.createDirectory(
            at: homeURL,
            withIntermediateDirectories: true
        )
        paneLayoutController.switchScope(to: selectedHostScope.paneScopeID)
        // The workspace's persisted Host identity pins the first bootstrap;
        // absent (never started), the first bootstrap pins it instead — the
        // same unknown-identity rule as a first SSH connect.
        let expectedHostID = MobilePairingStore.persistedHostID(
            at: homeURL
                .appendingPathComponent("mobile")
                .appendingPathComponent("mac-id")
        )
        // Workspace-pool handoff (same as selectHost): retire the pool's
        // read-only gateway for this workspace and seed its cached snapshot
        // so the switch renders real rows immediately.
        let pooledSeed = workspacePool.lendConnection(
            forKey: WorkspaceListOrder.localKey(home: normalizedHome)
        )
        // Same warm-adoption rule as selectHost: a still-active background
        // gateway connection for this exact workspace is reused, never
        // reopened.
        if !remoteHostRuntime.warmConnectionMatches(
            pinnedHostID: expectedHostID,
            workspaceHome: normalizedHome
        ) {
            remoteHostRuntime.connectLocalWorkspace(
                home: normalizedHome,
                name: name,
                expectedHostID: expectedHostID
            )
            if let pooledSeed {
                remoteHostRuntime.seedSnapshot(pooledSeed)
            }
        }
        projectRemoteScope(snapshot: remoteHostRuntime.snapshot)
        // Repaint the chrome to the scoped workspace's own color AND its
        // saved light/dark mode.
        applyScopeTint()
        refreshScopeAppearance()
        workspacePool.refreshTargets()
    }

    private func connectRemoteHost(
        _ host: PairedHostRecord,
        credentials: RemoteHostCredentials
    ) {
        // Pairing seals the Link key to this Controller id. A stale record
        // from another Controller identity must never be repurposed as a new
        // Link principal; repairing it means pairing again.
        guard host.controllerDeviceID == remoteHostStore.controllerIdentity.id else {
            remoteHostRuntime.requirePairingRepair()
            return
        }
        remoteHostRuntime.connectPairedHost(
            record: host,
            credentials: credentials
        )
    }

    private func connectSSHHost(_ host: SSHHostRecord) {
        remoteHostRuntime.connectSSH(
            target: host.target,
            expectedHostID: host.hostID,
            mode: host.mode,
            secret: host.usesStoredSecret ? remoteHostStore.sshSecret(for: host.id) : nil
        )
    }

    /// Validate, identify, and save an arbitrary SSH Host. Standard SSH is
    /// attempted first. If the provider rejects remote commands, the bounded
    /// interactive-shell gateway supports managed environments such as
    /// Upstash Box without changing the Host protocol above the transport.
    @discardableResult
    func addSSHHost(target input: String, secret: String?) async throws -> SSHHostRecord {
        let target = try Self.normalizedSSHTarget(input)
        let normalizedSecret = secret?.isEmpty == false ? secret : nil

        func probe(_ mode: RemoteSSHConnectionMode) async throws -> RemoteBootstrapSnapshot {
            let backend = try NativeRemoteBackend(
                sshTarget: target,
                mode: mode,
                secret: normalizedSecret
            )
            do {
                let snapshot = try await backend.bootstrap()
                await backend.close()
                return snapshot
            } catch {
                await backend.close()
                throw error
            }
        }

        let snapshot: RemoteBootstrapSnapshot
        let mode: RemoteSSHConnectionMode
        do {
            snapshot = try await probe(.command)
            mode = .command
        } catch {
            let standardMessage = error.localizedDescription
            do {
                snapshot = try await probe(.interactiveShell)
                mode = .interactiveShell
            } catch {
                throw SSHHostSetupError.connection(
                    standard: standardMessage,
                    interactive: error.localizedDescription
                )
            }
        }
        guard let hostID = snapshot.macID, !hostID.isEmpty else {
            throw SSHHostSetupError.missingIdentity
        }
        let fallbackName = String(target.dropFirst("ssh://".count))
        let name = snapshot.macName?.trimmingCharacters(in: .whitespacesAndNewlines)
        let record = try remoteHostStore.adoptSSH(
            target: target,
            name: name?.isEmpty == false ? name! : fallbackName,
            hostID: hostID,
            mode: mode,
            secret: normalizedSecret,
            select: false
        )
        selectHost(record.id, forceReconnect: true)
        return record
    }

    /// Install Unpeel with an explicit user action, then run the ordinary
    /// identity/bootstrap path. Standard SSH is attempted before the same
    /// interactive-shell fallback used by managed SSH providers.
    @discardableResult
    func installAndAddSSHHost(target input: String, secret: String?) async throws -> SSHHostRecord {
        let target = try Self.normalizedSSHTarget(input)
        let normalizedSecret = secret?.isEmpty == false ? secret : nil
        do {
            try await NativeRemoteBackend.installUnpeel(
                sshTarget: target,
                mode: .command,
                secret: normalizedSecret
            )
        } catch {
            let standardMessage = error.localizedDescription
            do {
                try await NativeRemoteBackend.installUnpeel(
                    sshTarget: target,
                    mode: .interactiveShell,
                    secret: normalizedSecret
                )
            } catch {
                throw SSHHostSetupError.installation(
                    standard: standardMessage,
                    interactive: error.localizedDescription
                )
            }
        }
        return try await addSSHHost(target: target, secret: normalizedSecret)
    }

    private static func normalizedSSHTarget(_ input: String) throws -> String {
        var value = input.trimmingCharacters(in: .whitespacesAndNewlines)
        if value.hasPrefix("ssh ") {
            value.removeFirst(4)
            if let comment = value.range(of: " #") {
                value = String(value[..<comment.lowerBound])
            }
        }
        if value.hasPrefix("ssh://") {
            value.removeFirst("ssh://".count)
        }
        value = value.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !value.isEmpty,
              !value.hasPrefix("-"),
              !value.contains(where: { $0.isWhitespace || $0.isNewline })
        else {
            throw SSHHostSetupError.invalidTarget
        }
        return "ssh://\(value)"
    }

    func forgetHost(_ hostID: String) {
        if selectedHostScope.remoteHostID == hostID {
            selectHost(nil)
        }
        // A forgotten Host must not keep a warm background connection alive
        // (selectHost(nil) deliberately preserves it for ordinary Local
        // round-trips). Match on both identities a record can carry: paired
        // records pin their hostID directly, SSH records select by record id
        // but pin their identified hostID.
        let sshPinnedID = remoteHostStore.sshRecords
            .first(where: { $0.id == hostID })?.hostID
        if remoteHostRuntime.warmConnectionMatches(pinnedHostID: hostID)
            || remoteHostRuntime.warmConnectionMatches(pinnedHostID: sshPinnedID) {
            remoteHostRuntime.disconnect()
        }
        remoteHostStore.forget(hostID: hostID)
        // Drop the forgotten Host's pooled connection and caches too.
        workspacePool.refreshTargets()
    }

    private func applyMenuTracking(_ delta: Int) {
        menuTrackingDepth = max(0, menuTrackingDepth + delta)
        if menuTrackingDepth == 0, rescanDeferredForMenuTracking {
            rescanDeferredForMenuTracking = false
            scheduleRescan(after: 0)
        }
        if menuTrackingDepth == 0 {
            scheduleDeferredRemoteProjectionFlush()
        }
    }

    private var sidebarInteractionBlocksRemoteProjection: Bool {
        menuTrackingDepth > 0
            || editingSessionID != nil
            || confirmingRemoveSessionID != nil
            || confirmingArchiveSessionID != nil
            || confirmingRemoveProjectID != nil
    }

    /// Schedule rather than flush synchronously from a published property's
    /// didSet. Some transitions clear one inline state immediately before
    /// setting another (archive confirm -> remove confirm); the next main-loop
    /// turn observes the settled state and cannot briefly remount the row.
    private func scheduleDeferredRemoteProjectionFlush() {
        guard remoteProjectionDeferredForSidebarInteraction else { return }
        DispatchQueue.main.async { [weak self] in
            guard let self,
                  self.remoteProjectionDeferredForSidebarInteraction,
                  self.selectedHostScope != .local
                    || self.localHostClientStarted,
                  !self.sidebarInteractionBlocksRemoteProjection
            else { return }
            self.remoteProjectionDeferredForSidebarInteraction = false
            self.projectRemoteScope(snapshot: self.remoteHostRuntime.snapshot)
        }
    }

    deinit {
        // The preset self-test creates throwaway stores; the FSEvents
        // context holds an unretained pointer to self, so the stream (and
        // the timers) must be torn down with the instance. Stores live and
        // die on the main actor.
        MainActor.assumeIsolated {
            teardownFileWatcher()
            localHostControlRefreshTask?.cancel()
            localHostControlRefreshTask = nil
            controllerPairingProxy?.stop()
            controllerPairingProxy = nil
            safetyTimer?.invalidate()
            workspacePool.stop()
            if let monitor = shortcutKeyMonitor { NSEvent.removeMonitor(monitor) }
            if let monitor = shortcutFlagsMonitor { NSEvent.removeMonitor(monitor) }
            // Block-based NotificationCenter observers are not auto-removed on
            // dealloc; drop them so throwaway self-test stores don't leave live
            // observers behind.
            for observer in appActivationObservers {
                NotificationCenter.default.removeObserver(observer)
            }
            appActivationObservers.removeAll()
        }
    }

    // MARK: - Hook events → activity + unread (App.svelte:638-680)

    /// Attach the app-owned hook server so launches export its port.
    /// Shared-core Phase 1: the app's own projects are mirrored into
    /// `app-state.json` so no project exists only in this UI's UserDefaults
    /// — the file is what the TUI (and a Linux host) read. UserDefaults
    /// stays the app-local working copy; the pre-existing merge already
    /// dedupes file-vs-record by path, so mirroring cannot double a project
    /// even under an older app build. Change-guarded: rescans call the
    /// write sites repeatedly and an unchanged mirror must cost a compare,
    /// not a file write that pings every peer.
    nonisolated static func projectSubtreeIDs(
        roots: Set<String>,
        parentByProjectID: [String: String]
    ) -> Set<String> {
        var result = roots
        var changed = true
        while changed {
            changed = false
            for (id, parentID) in parentByProjectID
            where result.contains(parentID) && !result.contains(id) {
                result.insert(id)
                changed = true
            }
        }
        return result
    }

    private func mirrorProjectsToSharedState() {
        let records = loadNativeProjects()
        let tombstoned = Set(
            AppDefaults.shared.stringArray(forKey: Self.removedProjectsKey) ?? []
        )
        guard let raw = try? Data(contentsOf: LaunchConfig.appStateFile),
              let object = (try? JSONSerialization.jsonObject(with: raw)) as? [String: Any]
        else { return } // never invent or clobber the file from here
        var projects = (object["projects"] as? [[String: Any]]) ?? []
        var changed = false
        let recordByID = Dictionary(uniqueKeysWithValues: records.map { ($0.id, $0) })
        let parentByProjectID = projects.reduce(into: [String: String]()) { result, entry in
            guard let id = entry["id"] as? String,
                  let parentID = entry["parent_project_id"] as? String
            else { return }
            result[id] = parentID
        }
        let tombstonedSubtree = Self.projectSubtreeIDs(
            roots: tombstoned,
            parentByProjectID: parentByProjectID
        )
        // Mirrored entries whose record is gone or tombstoned leave the file.
        projects.removeAll { entry in
            guard let id = entry["id"] as? String else { return false }
            let dead = tombstonedSubtree.contains(id)
                || (id.hasPrefix("native-") && recordByID[id] == nil)
            if dead { changed = true }
            return dead
        }
        for record in records where !tombstoned.contains(record.id) {
            var desired: [String: Any] = [
                "id": record.id, "name": record.name, "path": record.path,
            ]
            if let parent = record.parentProjectID { desired["parent_project_id"] = parent }
            if let branch = record.worktreeBranch { desired["worktree_branch"] = branch }
            if record.isFolder == true { desired["is_folder"] = true }
            if let index = projects.firstIndex(where: { ($0["id"] as? String) == record.id }) {
                // Update in place, preserving fields we don't model.
                var mergedEntry = projects[index]
                var rowChanged = false
                for (key, value) in desired
                where (mergedEntry[key] as? NSObject) != (value as? NSObject) {
                    mergedEntry[key] = value
                    rowChanged = true
                }
                if rowChanged {
                    projects[index] = mergedEntry
                    changed = true
                }
            } else if record.parentProjectID != nil || !projects.contains(where: { entry in
                guard let path = entry["path"] as? String else { return false }
                return Self.normalizedProjectPath(path) == Self.normalizedProjectPath(record.path)
            }) {
                // Another frontend already covers this folder → leave its
                // entry as the truth (ids then agree across UIs). Child
                // records (groups share the parent's path by design) skip
                // the path guard — only id identity matters for them.
                projects.append(desired)
                changed = true
            }
        }
        guard changed else { return }
        let snapshot = projects
        editPresetStateAnnouncing { object in object["projects"] = snapshot }
    }

    /// Apply only this app's pending pin intents to the latest shared object.
    /// `PresetStateFile.edit` invokes the mutation while holding the same
    /// app-state lock as Rust, so a TUI pin committed after our last scan is
    /// preserved instead of being erased by an outside-the-lock snapshot.
    @discardableResult
    private func mirrorPinsToSharedState(
        _ overrides: NativePinOverrides
    ) -> Bool {
        guard !overrides.added.isEmpty || !overrides.removedKeys.isEmpty else {
            return true
        }
        var applied = false
        let wrote = editPresetStateAnnouncing { object in
            applied = Self.applyPinOverrides(overrides, to: &object)
        }
        return applied && wrote
    }

    /// Raw-JSON mutation used inside the app-state lock. Unknown fields on
    /// unrelated pins (and on a moved existing pin) survive the rewrite. The
    /// legacy flat-array shape remains readable and is normalized to the
    /// current project-grouped shape on the first successful native intent.
    @discardableResult
    static func applyPinOverrides(
        _ overrides: NativePinOverrides,
        to object: inout [String: Any]
    ) -> Bool {
        let additions = Dictionary(
            overrides.added.map { ($0.key, $0) },
            uniquingKeysWith: { _, newest in newest }
        )
        let targetKeys = Set(overrides.removedKeys).union(additions.keys)
        guard !targetKeys.isEmpty else { return true }

        let rawPins = object["pinned_sessions"]
        var grouped: [String: [[String: Any]]] = [:]
        if rawPins == nil || rawPins is NSNull {
            grouped = [:]
        } else if let rawGroups = rawPins as? [String: Any] {
            for (projectID, rawRows) in rawGroups {
                guard let rows = rawRows as? [Any],
                      rows.allSatisfy({ $0 is [String: Any] })
                else { return false }
                grouped[projectID] = rows.compactMap { $0 as? [String: Any] }
            }
        } else if let rawRows = rawPins as? [Any] {
            // Legacy app-state.json stored one flat array.
            for rawRow in rawRows {
                guard let row = rawRow as? [String: Any],
                      let projectID = row["project_id"] as? String
                else { return false }
                grouped[projectID, default: []].append(row)
            }
        } else {
            // A corrupt/unknown shape must not be replaced with an empty map;
            // returning false keeps the UserDefaults intent for a later retry.
            return false
        }

        var priorRows: [String: [String: Any]] = [:]
        for projectID in grouped.keys {
            grouped[projectID] = grouped[projectID]?.filter { row in
                let key = (row["key"] as? String)
                    ?? (row["session_id"] as? String).map(
                        PinnedSidebarSession.key(forSessionID:)
                    )
                guard let key, targetKeys.contains(key) else { return true }
                if priorRows[key] == nil {
                    priorRows[key] = row
                }
                return false
            }
        }

        for pin in additions.values.sorted(by: { $0.key < $1.key }) {
            var row = priorRows[pin.key] ?? [:]
            row["key"] = pin.key
            row["project_id"] = pin.projectID
            if let sessionID = pin.sessionID {
                row["session_id"] = sessionID
            } else {
                row.removeValue(forKey: "session_id")
            }
            row["pinned_at"] = pin.pinnedAt
            grouped[pin.projectID, default: []].append(row)
        }
        object["pinned_sessions"] = grouped
        return true
    }

    /// Every preset write goes through here so the other frontends hear it
    /// — same rule as unpeel-core's app_state::save choke point.
    @discardableResult
    func editPresetStateAnnouncing(_ mutate: (inout [String: Any]) -> Void) -> Bool {
        let wrote = PresetStateFile.edit(mutate)
        if wrote {
            announceStateChange("app-state")
        }
        return wrote
    }

    // MARK: - Local-against-home (scoped workspace) state writes

    /// The `~/.unpeel` dir the CURRENT scope's filesystem/state verbs target:
    /// this instance's own home for `.local`, and the SCOPED workspace's home
    /// for `.localWorkspace`. A true remote scope has none — those verbs never
    /// run there.
    private var scopedLocalUnpeelDir: URL? {
        switch selectedHostScope {
        case .local:
            return LaunchConfig.unpeelDir
        case .localWorkspace(let home, _):
            return URL(fileURLWithPath: home, isDirectory: true)
        case .remote:
            return nil
        }
    }

    /// The `app-state.json` the current scope's local-against-home verbs write.
    private var scopedAppStateFile: URL? {
        scopedLocalUnpeelDir?.appendingPathComponent("app-state.json")
    }

    /// The UserDefaults suite that backs the current scope's native overlays
    /// (`unpeel.native.projects`, …): this instance's own `.shared` for
    /// `.local`, and the scoped workspace's derived suite for `.localWorkspace`
    /// so that workspace's own running app instance agrees on the projects.
    private var scopedAppDefaults: UserDefaults {
        AppDefaults.suite(forUnpeelHome: selectedHostScope.scopedLocalHome)
    }

    /// Edit the SCOPED workspace's `app-state.json` under the shared
    /// cross-process lock, then ping that home's peers (its own app instance /
    /// TUI) AND nudge the gateway to re-bootstrap so the change surfaces in the
    /// scoped sidebar without waiting for the 2s health poll. For `.local` this
    /// is exactly `editPresetStateAnnouncing`.
    @discardableResult
    private func editScopedAppStateAnnouncing(
        _ mutate: (inout [String: Any]) -> Void
    ) -> Bool {
        guard let url = scopedAppStateFile else { return false }
        if url == LaunchConfig.appStateFile {
            return editPresetStateAnnouncing(mutate)
        }
        let wrote = PresetStateFile.edit(at: url, mutate)
        guard wrote else { return false }
        // Ping the scoped home's OWN app-ports registry (its instance/TUI),
        // not this instance's — same registry the gateway/hooks broadcast to.
        Self.announceStateChange(
            "app-state",
            registry: scopedLocalUnpeelDir?.appendingPathComponent("app-ports"),
            ownPort: hookServer?.port
        )
        // Surface it in THIS window's scoped sidebar promptly.
        remoteHostRuntime.requestImmediateRefresh()
        return true
    }

    func attachHookServer(_ server: HookServer) {
        hookServer = server
        server.platformAdapterHandler = { [weak self] body, reply in
            Task { @MainActor in
                guard let self else {
                    reply(503, #"{"error":"app is shutting down"}"#)
                    return
                }
                switch HookServer.platformAdapterCall(from: body) {
                case .failure(.invalidEnvelope):
                    reply(400, #"{"error":"invalid platform adapter request"}"#)
                case .failure(.unsupportedOperation):
                    reply(501, #"{"error":"platform operation is not supported"}"#)
                case let .success(.presentApprovals(approvals)):
                    self.reconcileHostApprovals(approvals)
                    reply(200, #"{"ok":true}"#)
                case let .success(.openInEditor(path)):
                    Self.openFileInPreferredEditor(path: path, line: nil, column: nil)
                    reply(200, #"{"ok":true}"#)
                case let .success(.thumbnail(query)):
                    do {
                        let chunk = try MobileSessionControl.browserArtifactChunk(query: query)
                        guard let data = try? JSONEncoder().encode(chunk),
                              let body = String(data: data, encoding: .utf8)
                        else {
                            reply(500, #"{"error":"could not encode native thumbnail"}"#)
                            return
                        }
                        reply(200, body)
                    } catch let error as MobileRemoteError {
                        let data = try? JSONSerialization.data(withJSONObject: [
                            "error": error.message,
                        ])
                        reply(
                            error.status,
                            data.flatMap { String(data: $0, encoding: .utf8) }
                                ?? #"{"error":"native thumbnail failed"}"#
                        )
                    } catch {
                        reply(500, #"{"error":"native thumbnail failed"}"#)
                    }
                case .success(.computerStatus):
                    let experimental = self.currentWorkspaceSettingsWire().experimentalSettings
                    let available = experimental?.computerUseAvailable == true
                    var response: [String: Any] = [
                        "available": available,
                        "ready": available
                            && experimental?.computerUseReady == true
                            && FileManager.default.fileExists(
                                atPath: ComputerEngineManager.socketPath
                            ),
                    ]
                    if let reason = experimental?.computerUseUnavailableReason {
                        response["reason"] = reason
                    }
                    guard let data = try? JSONSerialization.data(withJSONObject: response),
                          let body = String(data: data, encoding: .utf8)
                    else {
                        reply(500, #"{"error":"could not encode Computer Use status"}"#)
                        return
                    }
                    reply(200, body)
                case let .success(.refreshLinkEntitlement(macID)):
                    let available = await RelayUplinkManager.shared
                        .refreshPlatformEntitlement(macID: macID)
                    reply(200, available
                        ? #"{"available":true}"#
                        : #"{"available":false}"#)
                case .success(.reconcileMobileE2EKeys):
                    let store = self.platformPairingStore
                    do {
                        try store.reconcilePlatformE2EKeys()
                        self.refreshPairedControllers()
                        reply(200, #"{"ok":true}"#)
                    } catch {
                        NSLog(
                            "[UnpeelNative] platform E2E Keychain reconciliation failed: %@",
                            error.localizedDescription
                        )
                        reply(500, #"{"error":"could not reconcile mobile Keychain"}"#)
                    }
                case let .success(.removeMobileE2EKey(deviceID)):
                    let store = self.platformPairingStore
                    do {
                        try store.removePlatformE2EKey(deviceID: deviceID)
                        self.refreshPairedControllers()
                        reply(200, #"{"ok":true}"#)
                    } catch {
                        NSLog(
                            "[UnpeelNative] platform E2E Keychain removal failed: %@",
                            error.localizedDescription
                        )
                        reply(409, #"{"error":"could not remove mobile Keychain key"}"#)
                    }
                case .success(.overlaySnapshot):
                    do {
                        reply(200, try NativeOverlaySnapshotAdapter.responseBody())
                    } catch {
                        NSLog(
                            "[UnpeelNative] platform overlay snapshot failed: %@",
                            error.localizedDescription
                        )
                        reply(500, #"{"error":"could not encode native overlay"}"#)
                    }
                case let .success(.setProjectFolderColor(projectID, colorID)):
                    // Rust already resolved the project and enforced the
                    // main-project/color policy. This callback owns only the
                    // workspace's native UserDefaults effect; suppress Host
                    // routing even if this window currently shows another
                    // Controller scope.
                    self.withNativeHostAdapterEffect {
                        self.setProjectFolderColor(
                            colorID.flatMap(ProjectFolderColor.init(rawValue:)),
                            for: projectID
                        )
                    }
                    reply(200, #"{"ok":true}"#)
                case let .success(.registerPushToken(deviceID, token, environment)):
                    let store = self.platformPairingStore
                    do {
                        guard try store.setPushToken(
                            deviceID: deviceID,
                            token: token,
                            environment: environment
                        ) != nil else {
                            reply(404, #"{"error":"unknown device"}"#)
                            return
                        }
                        self.refreshPairedControllers()
                        reply(200, #"{"ok":true}"#)
                    } catch {
                        NSLog("[UnpeelNative] platform push registration failed: %@", error.localizedDescription)
                        reply(500, #"{"error":"could not persist push token"}"#)
                    }
                case let .success(.recoverRelayCredentials(deviceID)):
                    let store = self.platformPairingStore
                    guard let credentials = store.rotateRelayCredentials(deviceID: deviceID) else {
                        reply(404, #"{"error":"unknown device"}"#)
                        return
                    }
                    guard let data = try? JSONEncoder().encode(credentials),
                          let body = String(data: data, encoding: .utf8)
                    else {
                        reply(500, #"{"error":"could not encode relay credentials"}"#)
                        return
                    }
                    reply(200, body)
                case let .success(.setNotifyWhenDone(sessionID, enabled)):
                    guard HookServer.isKnownSession(sessionID) else {
                        reply(404, #"{"error":"unknown session"}"#)
                        return
                    }
                    self.setLocalNotifyWhenDone(sessionID, enabled: enabled)
                    reply(200, #"{"ok":true}"#)
                case let .success(.deliverNotification(
                    sessionID,
                    title,
                    body,
                    rawKind,
                    requiresNotifyWhenDone,
                    sendDesktop,
                    suppressDeviceIDs
                )):
                    guard HookServer.isKnownSession(sessionID),
                          let kind = SessionPushKind(rawValue: rawKind)
                    else {
                        reply(404, #"{"error":"unknown session"}"#)
                        return
                    }
                    let macObserved = self.observedSessionID == sessionID
                    let eligible = !requiresNotifyWhenDone
                        || self.notifyWhenDoneSessionIDs.contains(sessionID)
                    if eligible {
                        if sendDesktop && !macObserved {
                            DesktopNotifier.shared.notify(
                                title: title,
                                body: body,
                                sessionID: sessionID,
                                kind: kind.rawValue
                            )
                        }
                        self.dispatchPhonePush(
                            sessionID: sessionID,
                            title: title,
                            body: body,
                            kind: kind,
                            suppressViewingTargets: true,
                            suppressedDeviceIDs: Set(suppressDeviceIDs)
                        )
                    }
                    reply(
                        200,
                        #"{"ok":true,"macObserved":\#(macObserved ? "true" : "false"),"eligible":\#(eligible ? "true" : "false")}"#
                    )
                }
            }
        }
        // Another frontend changed shared state: refresh now instead of
        // waiting for FSEvents coalescing or the safety-net rescan.
        server.stateChangeHandler = { [weak self] change in
            Task { @MainActor in
                guard let self else { return }
                Self.sharedOrderCache = nil
                Self.sharedProjectOrderCache = nil
                _ = change
                self.scheduleRescan(after: 0)
            }
        }
        // A peer workspace instance asked this one to come forward (its
        // selector switch); activation alone cannot reopen a closed window.
        server.showWindowHandler = {
            Task { @MainActor in
                NSApp.activate(ignoringOtherApps: true)
                (NSApp.delegate as? AppDelegate)?.showMainWindow()
            }
        }
        // A peer changed this workspace's App color from its per-line picker.
        server.reloadAppearanceHandler = { [weak self] in
            Task { @MainActor in self?.reloadAppTintFromDisk() }
        }
    }

    /// The session whose terminal the user is actually looking at:
    /// the selected session while the app is frontmost and the workspace is
    /// showing (getObservedWorkspaceSessionId, sessionUnread.ts:25-38 —
    /// shellView must be the terminal workspace, so Settings and the archive
    /// library both un-observe it).
    private var observedSessionID: String? {
        guard NSApp.isActive,
              !settingsVisible,
              archivedProjectID == nil,
              !recentActivityVisible
        else { return nil }
        if selectedHostScope == .local,
           let active = paneLayoutController.activePane,
           let selectedSessionID,
           paneLayoutState.group(containingSession: selectedSessionID)?.id
               == active.groupID,
           paneLayoutState.location(ofPane: active.paneID) == PaneLocation(
               groupID: active.groupID,
               paneID: active.paneID
           ) {
            return active.sessionID
        }
        return selectedSessionID
    }

    // MARK: - Host remote control

    /// Client-only Settings state comes from the canonical worker's local
    /// management contract. This is intentionally separate from the released
    /// Swift Host listener above.
    func startLocalHostControlClient() {
        guard UnpeelFeatureFlags.mobileRemoteControlEnabled else { return }
        startLocalHostControlRefresh()
    }

    func stopLocalHostControlClient() {
        cancelRemoteHostPairing()
        localHostControlRefreshTask?.cancel()
        localHostControlRefreshTask = nil
    }

    private func startLocalHostControlRefresh() {
        guard localHostControlRefreshTask == nil else { return }
        let home = LaunchConfig.unpeelDir.standardizedFileURL.path
        localHostControlRefreshTask = Task { @MainActor [weak self] in
            while !Task.isCancelled {
                guard let self else { return }
                await self.refreshLocalHostControlState(home: home)
                do {
                    try await Task.sleep(nanoseconds: 500_000_000)
                } catch {
                    return
                }
            }
        }
    }

    private func refreshLocalHostControlState(home: String? = nil) async {
        let home = home ?? LaunchConfig.unpeelDir.standardizedFileURL.path
        do {
            let snapshot = try await LocalHostControl.snapshot(home: home)
            hostManagement.apply(.init(
                devices: snapshot.devices, endpoint: snapshot.directEndpoint, error: nil
            ))
            if hostPairingPresentation.payload != nil {
                switch try await LocalHostControl.pairingStatus(home: home) {
                case .active:
                    break
                case .completed:
                    hostPairingPresentation = .paired
                case .closed:
                    hostPairingPresentation = .idle
                }
            }
        } catch {
            hostManagement.apply(.init(
                devices: pairedControllers, endpoint: nil,
                error: "Reconnecting to the workspace Host…"
            ))
            HostServiceManager.shared.ensureStarted()
        }
    }

    func beginHostPairing() {
        guard UnpeelFeatureFlags.mobileRemoteControlEnabled else { return }
        startLocalHostControlRefresh()
        hostServerError = nil
        let home = LaunchConfig.unpeelDir.standardizedFileURL.path
        Task { @MainActor [weak self] in
            do {
                let payload = try await LocalHostControl.beginPairing(home: home)
                guard let self else { return }
                self.hostPairingPresentation = .active(payload)
                await self.refreshLocalHostControlState()
            } catch {
                self?.hostServerError = error.localizedDescription
            }
        }
    }

    /// Pair a device with the SELECTED local workspace from this window:
    /// the workspace's own instance mints the code over the MCP-token
    /// loopback bridge (launching the instance first when it isn't running —
    /// a paired phone needs that instance serving anyway), and this window
    /// only displays it. The sibling's credentials never pass through here.
    func beginScopedWorkspacePairing() {
        guard case let .localWorkspace(home, name) = selectedHostScope else { return }
        scopedWorkspacePairingTask?.cancel()
        scopedWorkspacePairingGeneration &+= 1
        let generation = scopedWorkspacePairingGeneration
        scopedWorkspacePairingPresentation = .idle
        scopedWorkspacePairingError = nil
        scopedWorkspacePairingTask = Task { @MainActor [weak self] in
            do {
                let payload = try await Self.mintWorkspacePairingCode(home: home, name: name)
                guard let self, self.scopedWorkspacePairingGeneration == generation else { return }
                self.scopedWorkspacePairingPresentation = .active(payload)
            } catch is CancellationError {
            } catch {
                guard let self, self.scopedWorkspacePairingGeneration == generation else { return }
                self.scopedWorkspacePairingError = error.localizedDescription
            }
        }
    }

    func cancelScopedWorkspacePairing() {
        scopedWorkspacePairingTask?.cancel()
        scopedWorkspacePairingTask = nil
        scopedWorkspacePairingGeneration &+= 1
        scopedWorkspacePairingPresentation = .idle
        scopedWorkspacePairingError = nil
    }

    /// Ask the workspace instance's loopback bridge for a pairing code,
    /// launching the instance when no bridge answers and polling while it
    /// boots (a cold instance needs a few seconds to write `app-ports`).
    private static func mintWorkspacePairingCode(
        home: String,
        name: String
    ) async throws -> RemotePairingPayload {
        HostServiceManager.shared.ensureStarted()
        let deadline = Date().addingTimeInterval(25)
        var lastError: Swift.Error?
        while Date() < deadline {
            try Task.checkCancellation()
            do {
                return try await LocalHostControl.beginPairing(home: home)
            } catch {
                lastError = error
                try await Task.sleep(nanoseconds: 500_000_000)
            }
        }
        throw lastError ?? UnpeelWorkspaceError(
            "Could not reach \(name)'s workspace Host."
        )
    }

    /// Mint a phone pairing grant on the selected remote Host and relay only
    /// its sealed one-shot exchange through this Mac's LAN listener.
    func beginRemoteHostPairing() {
        guard let hostID = selectedHostScope.remoteHostID else {
            remoteHostPairingError = "Select a remote Host first."
            return
        }
        guard remoteHostRuntime.supportsHostOperation(
            RemoteHostRuntime.HostOperation.pairingInvitation
        ) else {
            remoteHostPairingError =
                "This Host cannot create remote pairing invitations yet. Update Unpeel on the Host."
            return
        }

        remoteHostPairingTask?.cancel()
        remoteHostPairingGeneration &+= 1
        let pairingGeneration = remoteHostPairingGeneration
        if let proxyID = remoteHostPairingProxyID {
            controllerPairingProxy?.cancel(id: proxyID)
        }
        remoteHostPairingProxyID = nil
        remoteHostPairingPresentation = .idle
        remoteHostPairingError = nil
        remoteHostPairingHostID = hostID

        if controllerPairingProxy == nil {
            controllerPairingProxy = ControllerPairingProxy()
        }
        guard let proxy = controllerPairingProxy else {
            remoteHostPairingError = "Waiting for this Mac's pairing listener. Try again in a moment."
            return
        }
        guard let reservation = proxy.reserve(provider: { [weak self] envelope in
            guard let self,
                  self.remoteHostPairingGeneration == pairingGeneration,
                  self.selectedHostScope.remoteHostID == hostID,
                  self.remoteHostPairingHostID == hostID
            else {
                throw MobileRemoteError(409, "the selected Host changed")
            }
            let response = try await self.remoteHostRuntime.completePairingInvitation(
                envelopeJSON: envelope
            )
            guard self.remoteHostPairingGeneration == pairingGeneration,
                  self.selectedHostScope.remoteHostID == hostID
            else {
                throw MobileRemoteError(409, "the selected Host changed")
            }
            let pairingToken = self.remoteHostPairingPayload?.token ?? ""
            self.remoteHostPairingPresentation = self.remoteHostPairingPresentation.completing(
                token: pairingToken
            )
            self.remoteHostPairingProxyID = nil
            return response
        }) else {
            remoteHostPairingError = "Could not open a pairing proxy on this Mac."
            return
        }
        remoteHostPairingProxyID = reservation.id

        remoteHostPairingTask = Task { @MainActor [weak self] in
            guard let self else { return }
            defer {
                if self.remoteHostPairingGeneration == pairingGeneration {
                    self.remoteHostPairingTask = nil
                }
            }
            do {
                let payload = try await self.remoteHostRuntime.createPairingInvitation(
                    proxyEndpoint: reservation.endpoint
                )
                try Task.checkCancellation()
                guard self.remoteHostPairingGeneration == pairingGeneration,
                      self.selectedHostScope.remoteHostID == hostID,
                      self.remoteHostPairingProxyID == reservation.id
                else { return }
                self.remoteHostPairingPresentation = .active(payload)
            } catch is CancellationError {
                proxy.cancel(id: reservation.id)
            } catch {
                proxy.cancel(id: reservation.id)
                guard self.remoteHostPairingGeneration == pairingGeneration else { return }
                if self.remoteHostPairingProxyID == reservation.id {
                    self.remoteHostPairingProxyID = nil
                }
                self.remoteHostPairingError = error.localizedDescription
            }
        }
    }

    func cancelRemoteHostPairing() {
        remoteHostPairingTask?.cancel()
        remoteHostPairingTask = nil
        remoteHostPairingGeneration &+= 1
        if let proxyID = remoteHostPairingProxyID {
            controllerPairingProxy?.cancel(id: proxyID)
        }
        controllerPairingProxy?.stop()
        controllerPairingProxy = nil
        remoteHostPairingProxyID = nil
        remoteHostPairingHostID = nil
        remoteHostPairingPresentation = .idle
        remoteHostPairingError = nil
    }

    func cancelHostPairing() {
        let home = LaunchConfig.unpeelDir.standardizedFileURL.path
        Task { @MainActor [weak self] in
            try? await LocalHostControl.cancelPairing(home: home)
            self?.hostPairingPresentation = .idle
            await self?.refreshLocalHostControlState(home: home)
        }
    }

    func completeHostPairing(token: String) {
        hostPairingPresentation = hostPairingPresentation.completing(token: token)
    }

    func refreshPairedControllers() {
        Task { @MainActor [weak self] in
            await self?.refreshLocalHostControlState()
        }
    }

    var mobilePushTargetCount: Int {
        platformPairingStore.pushTargets().count
    }

    func revokeMobileDevice(_ deviceID: String) {
        let home = LaunchConfig.unpeelDir.standardizedFileURL.path
        Task { @MainActor [weak self] in
            do {
                try await LocalHostControl.revokeDevice(home: home, id: deviceID)
                await self?.refreshLocalHostControlState(home: home)
            } catch {
                self?.hostServerError = error.localizedDescription
            }
        }
    }

    /// Scope a paired Controller to Direct-only (allowed = false) or enroll
    /// it on Unpeel Link. Enforcement is Host-side: the uplink re-registers
    /// its device token set on the change notification.
    func setDeviceRelayAllowed(_ deviceID: String, _ allowed: Bool) {
        let home = LaunchConfig.unpeelDir.standardizedFileURL.path
        Task { @MainActor [weak self] in
            do {
                try await LocalHostControl.setRelayAllowed(
                    home: home,
                    id: deviceID,
                    allowed: allowed
                )
                if allowed {
                    // Downgrade compatibility: older builds still gate the
                    // uplink on the retired global toggle's key.
                    AppDefaults.shared.set(true, forKey: RelayConfig.enabledDefaultsKey)
                }
                await self?.refreshLocalHostControlState(home: home)
            } catch {
                self?.hostServerError = error.localizedDescription
            }
        }
    }

    /// Scope a paired Host (outbound) to Direct-only or restore its Unpeel
    /// Link fallback. If that Host is the current scope, reconnect so the
    /// active connection plan matches the new enrollment immediately —
    /// otherwise a live Direct connection could still fall back to Link
    /// later (or a narrowed Host could keep an open Link route).
    func setHostLinkEnabled(_ hostID: String, _ enabled: Bool) {
        remoteHostStore.setLinkEnabled(enabled, forHost: hostID)
        if selectedHostScope.remoteHostID == hostID {
            selectHost(hostID, forceReconnect: true)
        }
    }

    /// The workspace's current behavior knobs for the bootstrap wire
    /// (`settings.workspace.set`'s read half). Values come from the shared
    /// file with the same fallbacks every consumer applies.
    func currentWorkspaceSettingsWire() -> RemoteWorkspaceSettings {
        let raw = (try? Data(contentsOf: LaunchConfig.appStateFile))
            .flatMap { try? JSONSerialization.jsonObject(with: $0) as? [String: Any] } ?? [:]
        func access(_ key: String, _ fallback: String) -> String {
            raw[key] as? String ?? fallback
        }
        let transcripts = transcriptSettings
        let transparency = TransparencyModel.savedValues(in: AppDefaults.shared)
        let computerAdapterAvailable = UnpeelFeatureFlags.computerUseAvailable
            && ComputerPermissions.resolveEngine() != nil
        let computerFeatureEnabled = isExperimentalEnabled(.computerUse)
        let computerAdapterReady = computerFeatureEnabled
            && computerDefaultAccess != .off
            && ComputerEngineManager.shared.isRunning
        let computerAdapterReason: String? = if !UnpeelFeatureFlags.computerUseAvailable {
            "Computer use is not included in this Host build."
        } else if ComputerPermissions.resolveEngine() == nil {
            "Cua Driver is not installed on this Host."
        } else if computerFeatureEnabled && computerDefaultAccess == .off {
            "Computer access is Off in Settings ▸ Computer use."
        } else if computerFeatureEnabled && !computerAdapterReady {
            "Cua Driver is starting or could not start on this Host."
        } else {
            nil
        }
        return RemoteWorkspaceSettings(
            transcriptSettings: RemoteTranscriptSettings(
                includeUser: transcripts.includeUser,
                includeAssistant: transcripts.includeAssistant,
                includeReasoning: transcripts.includeReasoning,
                includeTools: transcripts.includeTools,
                includeFileChanges: transcripts.includeFileChanges,
                includePlanUpdates: transcripts.includePlanUpdates,
                includeSessionInfo: transcripts.includeSessionInfo,
                maxEntries: transcripts.maxEntries
            ),
            appearanceSettings: RemoteAppearanceSettings(
                theme: themePreference.rawValue,
                appTint: appTint.rawValue,
                backgroundOpacity: transparency.background,
                surfaceOpacity: transparency.surface,
                backgroundTone: transparency.backgroundTone,
                surfaceTone: transparency.surfaceTone,
                sessionTitleMode: sessionTitleMode.rawValue
            ),
            notificationSettings: RemoteNotificationSettings(
                menuAttentionDetection: menuAttentionDetectionEnabled
            ),
            experimentalSettings: RemoteExperimentalSettings(
                worktrees: isExperimentalEnabled(.worktrees),
                sessionsMcp: isExperimentalEnabled(.sessionsMcp),
                browserMcp: isExperimentalEnabled(.browserMcp),
                computerUse: computerFeatureEnabled,
                computerUseAvailable: computerAdapterAvailable,
                computerUseReady: computerAdapterReady,
                computerUseUnavailableReason: computerAdapterReason,
                workspaces: isExperimentalEnabled(.workspaces)
            ),
            autoStopArchiveMinutes: autoStopArchiveMinutes,
            sidebarStoppedLimit: sidebarVisibleSessionLimit,
            browserDefaultAccess: access("browser_default_access", "on"),
            mcpNonchildWriteAccess: access("mcp_nonchild_write_access", "ask"),
            computerAccess: access(
                "computer_default_access",
                raw["computer_access"] as? String ?? "ask"
            ),
            mcpWorktreeAccess: raw["mcp_worktree_access"] as? Bool ?? false,
            mcpAutoAddBrowserScreenshots: raw["mcp_auto_add_browser_screenshots"] as? Bool ?? true
        )
    }

    /// Move one preset to `index` in the flat display order — the remote
    /// counterpart of a Presets-panel drag, through the same order writer.
    private func movePresetTo(id: String, index: Int) {
        let current = mergedPresets.map(\.id)
        guard let from = current.firstIndex(of: id) else { return }
        var order = current
        order.remove(at: from)
        order.insert(id, at: min(index, order.count))
        // A no-op move skips the write (and its state-bus announce).
        if order != current {
            movePresets(
                current,
                from: IndexSet(integer: from),
                to: min(index, order.count) > from ? min(index, order.count) + 1
                    : min(index, order.count)
            )
        }
    }

    /// This Host's own displayed sibling order — the local counterpart of
    /// `projectOrderIDs`, which reads the scope-selected display tree.
    private func localProjectOrderIDs(parentID: String?) -> [String] {
        guard let parentID else { return nodes.map(\.id) }
        return findNode(parentID)?.worktrees.map(\.id) ?? []
    }

    /// Opt a session in/out of the "finished" push notification. Persisted to
    /// the native overlay; the next remote snapshot reflects it, and the push
    /// dispatcher reads it when the session settles.
    func setNotifyWhenDone(_ sessionID: String, enabled: Bool) {
        if routesSessionVerbThroughHost(sessionID) {
            performRemoteVerb("Couldn't update the notification") { runtime in
                try await runtime.setNotifyWhenDone(sessionID, enabled: enabled)
            }
            return
        }
        setLocalNotifyWhenDone(sessionID, enabled: enabled)
    }

    /// Native adapter half. It is deliberately independent of the selected
    /// Host scope: this app remains the registered platform adapter for its
    /// own workspace while its window is controlling another Host.
    private func setLocalNotifyWhenDone(_ sessionID: String, enabled: Bool) {
        let has = notifyWhenDoneSessionIDs.contains(sessionID)
        guard has != enabled else { return }
        if enabled {
            notifyWhenDoneSessionIDs.insert(sessionID)
        } else {
            notifyWhenDoneSessionIDs.remove(sessionID)
        }
        AppDefaults.shared.set(
            Array(notifyWhenDoneSessionIDs), forKey: NativeOverlay.notifyWhenDoneKey
        )
    }

    /// Force-clear a session's attention badge ("Clear attention" in the
    /// sidebar context menu) — the escape hatch for a stuck or false badge.
    /// Covers both sources: a hook-owned PermissionRequest state drops to
    /// idle (later hook events re-drive it as usual), and the host's
    /// menu-prompt flag is dismissed until it lowers and re-raises.
    func clearAttention(_ sessionID: String) {
        guard selectedHostScope == .local else { return }
        activity.clearAttention(sessionID)
        menuAttentionDismissals.insert(sessionID)
        rescan()
    }

    // MARK: - Archive (non-destructive "clear it out")

    /// Session ids showing the inline "Stop & archive?" confirmation row —
    /// only sessions that are actively working (busy/starting/attention)
    /// confirm; idle and exited sessions archive directly.
    @Published private(set) var confirmingArchiveSessionID: String? {
        didSet {
            if confirmingArchiveSessionID == nil {
                scheduleDeferredRemoteProjectionFlush()
            }
        }
    }

    /// Archive entry point for the UI: working sessions get an inline
    /// confirm (archiving kills the turn mid-flight); settled ones archive
    /// straight away. Launches without a provider conversation/storage route
    /// to Remove instead; neither user action nor inactivity cleanup may put
    /// a non-resumable Session in Archive.
    func requestArchiveSession(_ sessionID: String) {
        guard let session = displaySessionsByID[sessionID] else { return }
        guard sessionCanArchive(sessionID) else {
            requestRemoveSession(sessionID)
            return
        }
        confirmingRemoveSessionID = nil
        switch session.status {
        case .starting, .busy, .attention:
            confirmingArchiveSessionID = sessionID
        case .idle, .exited:
            archiveSession(sessionID)
        }
    }

    func cancelArchiveConfirm() {
        confirmingArchiveSessionID = nil
    }

    /// Stop-and-file-away: kill the hosted PTY (same identity-guarded path as
    /// Remove) and reap the session's browser daemon, but keep the session
    /// dir — manifest, output.bin, artifacts, provider-id overlay — intact,
    /// then hide the row from the sidebar. The project's archived-session
    /// view can resume the exact conversation via ResumeCommand.
    /// `stampRecency` is true for user-initiated archives (the stamp floats
    /// the row to the top of the archive section); automatic inactivity
    /// cleanup passes false so old sessions file away without resurfacing.
    // MARK: - Shared sidebar order (cross-frontend contract)

    /// `~/.unpeel/session-order.json` — `{ project_id: [session ids] }`, the
    /// same hand-ordering this app keeps per project in UserDefaults. A drag
    /// in the TUI lands here; a drag here lands there.
    static var sharedSessionOrderURL: URL {
        LaunchConfig.unpeelDir.appendingPathComponent("session-order.json")
    }

    /// Parsed `session-order.json`, cached against the file's modification
    /// date. This is consulted for every project on every sidebar rebuild,
    /// so the common "nothing changed" case must cost a stat, not a parse.
    private nonisolated(unsafe) static var sharedOrderCache: (stamp: Date, root: [String: [String]])?

    static func sharedSessionOrder(projectID: String) -> [String]? {
        let url = sharedSessionOrderURL
        let stamp = (try? FileManager.default.attributesOfItem(atPath: url.path))?[.modificationDate] as? Date
        guard let stamp else {
            sharedOrderCache = nil
            return nil
        }
        if sharedOrderCache?.stamp != stamp {
            var parsed: [String: [String]] = [:]
            if let data = try? Data(contentsOf: url),
               let root = (try? JSONSerialization.jsonObject(with: data)) as? [String: Any] {
                for (key, value) in root {
                    if let ids = value as? [String] { parsed[key] = ids }
                }
            }
            sharedOrderCache = (stamp, parsed)
        }
        let ids = sharedOrderCache?.root[projectID] ?? []
        return ids.isEmpty ? nil : ids
    }

    @discardableResult
    private static func editSharedSessionOrders(
        _ edit: (inout [String: Any]) -> Bool
    ) -> Bool {
        // Read-modify-write on a cross-frontend file: take the same lock
        // the Rust writer does, or a concurrent TUI drag loses this edit.
        let wrote = PresetStateFile.withExclusiveLock(on: sharedSessionOrderURL) {
            var root: [String: Any]
            if let data = try? Data(contentsOf: sharedSessionOrderURL) {
                guard let parsed = (try? JSONSerialization.jsonObject(with: data))
                        as? [String: Any]
                else { return false }
                root = parsed
            } else {
                root = [:]
            }
            guard edit(&root), JSONSerialization.isValidJSONObject(root),
                  let data = try? JSONSerialization.data(withJSONObject: root)
            else { return false }
            do {
                try FileManager.default.createDirectory(
                    at: sharedSessionOrderURL.deletingLastPathComponent(),
                    withIntermediateDirectories: true
                )
                try data.write(to: sharedSessionOrderURL, options: .atomic)
                return true
            } catch {
                NSLog("[UnpeelNative] shared session order write failed: \(error)")
                return false
            }
        } ?? false
        // Never let this process's cached pre-write value win the rebuild that
        // immediately follows a local commit.
        sharedOrderCache = nil
        return wrote
    }

    @discardableResult
    static func writeSharedSessionOrder(projectID: String, ids: [String]) -> Bool {
        editSharedSessionOrders { root in
            if ids.isEmpty {
                root.removeValue(forKey: projectID)
            } else {
                root[projectID] = ids
            }
            return true
        }
    }

    @discardableResult
    static func removeSessionFromSharedOrders(_ sessionID: String) -> Bool {
        editSharedSessionOrders { root in
            var changed = false
            for key in Array(root.keys) {
                guard let ids = root[key] as? [String], ids.contains(sessionID) else {
                    continue
                }
                let kept = ids.filter { $0 != sessionID }
                if kept.isEmpty {
                    root.removeValue(forKey: key)
                } else {
                    root[key] = kept
                }
                changed = true
            }
            return changed
        }
    }

    // MARK: - Shared session markers (cross-frontend contract)

    /// Session-dir markers any frontend can write: `archived.json`,
    /// `title.json`, `read.json`. The TUI and CLI have no access to this
    /// app's UserDefaults overlays, so these files are how the desktop, a
    /// headless host, and the phone agree on organization state. The
    /// Shared markers are authoritative once present. UserDefaults overlays
    /// remain only as migration/write-failure fallbacks for older builds.
    enum SharedMarker: String {
        case archived = "archived.json"
        case title = "title.json"
        case read = "read.json"
        /// Hook-captured provider conversation metadata — see
        /// unpeel-core session_ops::set_provider_session for the merge and
        /// no-announce semantics both sides follow.
        case providerSession = "provider-session.json"
        /// The user filed the session under another project (group or
        /// worktree folder): `{"project_id": "<target>", "moved_at": ms}`.
        /// Any frontend may write or delete it; a missing/stale target
        /// falls back to the manifest project.
        case projectOverride = "project-override.json"
    }

    nonisolated static func sharedMarkerURL(
        _ sessionID: String, _ marker: SharedMarker
    ) -> URL {
        LaunchConfig.appSessionsDir
            .appendingPathComponent(sessionID)
            .appendingPathComponent(marker.rawValue)
    }

    /// Marker path under an EXPLICIT home — the against-the-home twin used by
    /// scoped local-workspace verbs (same write class as Add Project).
    nonisolated static func sharedMarkerURL(
        _ sessionID: String, _ marker: SharedMarker, home: URL
    ) -> URL {
        home.appendingPathComponent("app-sessions")
            .appendingPathComponent(sessionID)
            .appendingPathComponent(marker.rawValue)
    }

    @discardableResult
    nonisolated static func writeSharedMarker(
        _ sessionID: String, _ marker: SharedMarker, _ body: [String: Any], home: URL
    ) -> Bool {
        let url = sharedMarkerURL(sessionID, marker, home: home)
        guard FileManager.default.fileExists(
            atPath: url.deletingLastPathComponent().path
        ),
            let data = try? JSONSerialization.data(withJSONObject: body)
        else { return false }
        do {
            try data.write(to: url, options: .atomic)
            return true
        } catch {
            return false
        }
    }

    nonisolated static func removeSharedMarker(
        _ sessionID: String, _ marker: SharedMarker, home: URL
    ) {
        try? FileManager.default.removeItem(
            at: sharedMarkerURL(sessionID, marker, home: home)
        )
    }

    /// Exact counterpart of Rust `session_ops::lifecycle_lock_target_at`
    /// followed by `app_state::lock_exclusive`'s `.lock` extension.
    nonisolated static func sessionLifecycleLockURL(
        unpeelDir: URL, sessionID: String
    ) -> URL {
        let digest = SHA256.hash(data: Data(sessionID.utf8))
            .map { String(format: "%02x", $0) }
            .joined()
        return unpeelDir
            .appendingPathComponent("session-lifecycle-locks", isDirectory: true)
            .appendingPathComponent("\(digest).lock")
    }

    /// Acquire without waiting. Lifecycle actions run from MainActor entry
    /// points, so contention is a retryable rejection, never a UI stall.
    nonisolated static func acquireSessionLifecycleLease(
        unpeelDir: URL, sessionID: String
    ) -> NativeSessionFileLockLease? {
        acquireSessionFileLock(
            at: sessionLifecycleLockURL(unpeelDir: unpeelDir, sessionID: sessionID)
        )
    }

    nonisolated static func replacementRestartAllowsState(
        _ manifestState: String?,
        stoppedOnly: Bool,
        childProcessExists: Bool?,
        pidIdentity: ManifestPidIdentity
    ) -> Bool {
        guard stoppedOnly else { return true }
        if manifestState == "exited" { return true }
        guard manifestState == "running" else { return false }
        // A crashed Host can leave its final manifest at `running`. Resume is
        // safe only when the recorded child is definitely absent, or its pid
        // has definitely been recycled onto an unrelated process. Unknown
        // identity plus a live/unknown pid must fail closed.
        return childProcessExists == false || pidIdentity == .notOurs
    }

    /// `kill(pid, 0)` existence probe with EPERM treated as alive. Nil means
    /// the manifest did not provide a valid pid, which is unknown rather than
    /// proof that the child died.
    nonisolated static func hostedChildProcessExists(_ pid: Int32?) -> Bool? {
        guard let pid, pid > 1 else { return nil }
        if kill(pid, 0) == 0 { return true }
        switch errno {
        case EPERM: return true
        case ESRCH: return false
        default: return nil
        }
    }

    private nonisolated static func acquireSessionFileLock(
        at lockURL: URL
    ) -> NativeSessionFileLockLease? {
        do {
            try FileManager.default.createDirectory(
                at: lockURL.deletingLastPathComponent(),
                withIntermediateDirectories: true
            )
        } catch {
            NSLog("[UnpeelNative] failed to create Session lock directory: \(error)")
            return nil
        }
        let descriptor = open(
            lockURL.path,
            O_CREAT | O_RDWR | O_CLOEXEC,
            mode_t(0o600)
        )
        guard descriptor >= 0 else { return nil }
        guard fchmod(descriptor, mode_t(0o600)) == 0,
              flock(descriptor, LOCK_EX | LOCK_NB) == 0
        else {
            close(descriptor)
            return nil
        }
        return NativeSessionFileLockLease(descriptor: descriptor)
    }

    private nonisolated static func sharedMarkerExistsUnlocked(
        _ sessionID: String, _ marker: SharedMarker
    ) -> Bool {
        // Raw access(2) on a concatenated path, not FileManager + URL:
        // rescan runs this for every session dir several times per second,
        // and the ObjC/URL machinery made this ~10x the syscall cost —
        // visible as main-thread stall in scroll/animation profiles.
        let path = LaunchConfig.appSessionsDir.path
            + "/" + sessionID + "/" + marker.rawValue
        return access(path, F_OK) == 0
    }

    /// Existence check only — a stat instead of an open+parse. Rescan asks
    /// this for every live session, so the common "no marker" case must not
    /// cost a file read.
    nonisolated static func sharedMarkerExists(
        _ sessionID: String, _ marker: SharedMarker
    ) -> Bool {
        sharedMarkerExistsUnlocked(sessionID, marker)
    }

    nonisolated static func readSharedMarker(
        _ sessionID: String, _ marker: SharedMarker
    ) -> [String: Any]? {
        let read: () -> [String: Any]? = {
            guard sharedMarkerExistsUnlocked(sessionID, marker),
                  let data = try? Data(contentsOf: sharedMarkerURL(sessionID, marker))
            else { return nil }
            return (try? JSONSerialization.jsonObject(with: data)) as? [String: Any]
        }
        return read()
    }

    @discardableResult
    nonisolated static func writeSharedMarker(
        _ sessionID: String, _ marker: SharedMarker, _ body: [String: Any]
    ) -> Bool {
        let write = {
            let url = sharedMarkerURL(sessionID, marker)
            guard FileManager.default.fileExists(atPath: url.deletingLastPathComponent().path),
                  let data = try? JSONSerialization.data(withJSONObject: body)
            else { return false }
            do {
                try data.write(to: url, options: .atomic)
                return true
            } catch {
                return false
            }
        }
        return write()
    }

    nonisolated static func removeSharedMarker(
        _ sessionID: String, _ marker: SharedMarker
    ) {
        let remove = {
            try? FileManager.default.removeItem(at: sharedMarkerURL(sessionID, marker))
        }
        remove()
    }

    /// Resolve the latest real activity signal with the same provider-aware
    /// rule as unpeel-core. Hook-capable agents have a truthful durable hook
    /// seed; when that seed is absent they have not produced a lifecycle
    /// event yet, so a TUI repaint in output.bin must NOT make them recent.
    /// Hookless tools use the host's parsed-screen change stamp, falling back
    /// to output.bin only for manifests from hosts that predate that field.
    static func resolvedLastRealActivityAtMs(
        command: String,
        hookEventAtMs: Int64?,
        screenChangedAtMs: Int64?,
        outputAtMs: Int64?
    ) -> Int64? {
        if SetupTool.detect(in: command)?.usesLifecycleHooks == true {
            return hookEventAtMs
        }
        return screenChangedAtMs ?? outputAtMs
    }

    /// Canonical timestamp used by Recent/date ordering. Creation is the
    /// start event and therefore the floor. The host's `updated_at` joins the
    /// rank only after it writes an exited manifest; while running that field
    /// is a heartbeat and would otherwise float every live session to now.
    static func resolvedLifecycleAtMs(
        createdAtMs: Int64,
        command: String,
        hookEventAtMs: Int64?,
        screenChangedAtMs: Int64?,
        outputAtMs: Int64?,
        finalExitedAtMs: Int64?
    ) -> Int64 {
        max(
            max(
                createdAtMs,
                resolvedLastRealActivityAtMs(
                    command: command,
                    hookEventAtMs: hookEventAtMs,
                    screenChangedAtMs: screenChangedAtMs,
                    outputAtMs: outputAtMs
                ) ?? 0
            ),
            finalExitedAtMs ?? 0
        )
    }

    private nonisolated static func fileModificationAtMs(_ path: String) -> Int64? {
        guard let stamp = statFile(path) else { return nil }
        return Int64(stamp.mtimeSec) * 1_000 + Int64(stamp.mtimeNsec) / 1_000_000
    }

    /// Filesystem-backed last-real-activity read for unread-marker
    /// reconciliation. Keep this command-aware: a missing hook seed on
    /// Claude/Codex/etc intentionally returns nil rather than consulting
    /// screen/output repaint signals.
    nonisolated static func sessionLastRealActivityAtMs(
        _ sessionID: String, command: String
    ) -> Int64? {
        let dir = LaunchConfig.appSessionsDir.appendingPathComponent(sessionID).path
        if SetupTool.detect(in: command)?.usesLifecycleHooks == true {
            return fileModificationAtMs(dir + "/last-hook-event.json")
        }
        var screenChangedAt: Int64?
        if let raw = try? Data(contentsOf: URL(fileURLWithPath: dir + "/manifest.json")),
           let json = try? JSONSerialization.jsonObject(with: raw) as? [String: Any],
           let stamp = (json["screen_changed_at"] as? NSNumber)?.int64Value,
           stamp > 0 {
            screenChangedAt = stamp
        }
        return screenChangedAt ?? fileModificationAtMs(dir + "/output.bin")
    }

    /// Unified Recent recency for ⌘K and sidebar date mode: the latest
    /// lifecycle event or App alert, with creation as its floor. Read receipts
    /// are not activity — selecting/reading a row must never reshuffle any
    /// Recent surface. Ctrl-Tab's explicit MRU stays a separate model.
    func sessionRecencyMs(_ sessionID: String) -> Int64 {
        guard let session = sessionsByID[sessionID] else { return 0 }
        return max(
            max(session.createdAt, session.lifecycleAtMs ?? 0),
            latestAlertAtMs(for: sessionID) ?? 0
        )
    }

    /// Recency for every listable (non-archived) session, snapshotted once
    /// per ⌘K open so every keystroke filters one stable ordering.
    func paletteRecencySnapshot() -> [String: Int64] {
        var snapshot: [String: Int64] = [:]
        func collect(_ nodes: [ProjectNode]) {
            for node in nodes {
                for session in node.sessions
                where !archivedSessionIDs.contains(session.id) {
                    snapshot[session.id] = sessionRecencyMs(session.id)
                }
                collect(node.worktrees)
            }
        }
        collect(nodes)
        return snapshot
    }

    func archiveSession(_ sessionID: String, stampRecency: Bool = true) {
        guard sessionCanArchive(sessionID) else { return }
        if confirmingArchiveSessionID == sessionID {
            confirmingArchiveSessionID = nil
        }
        performRemoteVerb("Couldn't archive the session") { runtime in
            try await runtime.archiveSession(sessionID)
        }
    }

    /// Put an archived session back in the regular list (as a restartable
    /// exited row — archive stopped its host).
    func unarchiveSession(_ sessionID: String) {
        performRemoteVerb("Couldn't restore the session") { runtime in
            try await runtime.restoreSession(sessionID)
        }
    }

    /// Restore without starting: return the row to its project, close the
    /// archive library, and make sure the sidebar can actually show it.
    func restoreArchivedSessionToSidebar(_ sessionID: String) {
        // Remote archive page: restore on the Host; the row returns to the
        // sidebar on the next bootstrap.
        unarchiveSession(sessionID)
        archivedProjectID = nil
    }

    /// Resume an archived provider conversation and take the user straight
    /// back to its terminal. `restartSession` mints the replacement id and
    /// prunes the archived flag from the old id during teardown.
    @discardableResult
    func resumeArchivedSession(_ sessionID: String) -> Bool {
        // Remote archive page: restore + restart on the Host as one flow.
        guard let source = remoteSummary(for: sessionID) else { return false }
        let knownSessionIDs = Set(remoteSummariesByID.keys)
            .union(remoteArchivedSummaryCache.sessionIDs)
        guard !restartingSessionIDs.contains(sessionID) else { return false }
        archivedProjectID = nil
        beginRemoteRestartPlaceholder(sessionID)
        performRemoteVerb("Couldn't resume the session", onFailure: { [weak self] in
            self?.endRemoteRestartPlaceholder(sessionID)
        }) { runtime in
            try await runtime.restoreAndRestartSession(
                source,
                knownSessionIDs: knownSessionIDs
            )
        }
        return true
    }

    private func persistArchivedSessionIDs() {
        if archivedSessionIDs.isEmpty {
            AppDefaults.shared.removeObject(forKey: NativeOverlay.archivedSessionsKey)
        } else {
            AppDefaults.shared.set(
                Array(archivedSessionIDs), forKey: NativeOverlay.archivedSessionsKey
            )
        }
        // The recency stamps live and die with the archived flag.
        archivedAtBySession = archivedAtBySession.filter {
            archivedSessionIDs.contains($0.key)
        }
        if archivedAtBySession.isEmpty {
            AppDefaults.shared.removeObject(forKey: NativeOverlay.archivedAtKey)
        } else {
            AppDefaults.shared.set(
                archivedAtBySession.mapValues { NSNumber(value: $0) },
                forKey: NativeOverlay.archivedAtKey
            )
        }
    }

    // MARK: - Settings screen (App.svelte openSettings/closeSettings:442-456)

    /// `tab: nil` keeps the current tab, so the gear/⌘, on an already-open
    /// settings view doesn't yank the user back to Presets.
    /// NOTE: never wrap settingsVisible changes in withAnimation — the
    /// sidebar slide is driven by a scoped `.animation(value:)` modifier in
    /// SidebarView, and the content-pane swap (ContentArea) must stay
    /// non-animated so the Metal-backed terminal surface is never part of
    /// an opacity/frame animation (that's what caused the settings blink).
    func openSettings(tab: SettingsTab? = nil) {
        if let tab {
            settingsTab = (tab == .mobile && !UnpeelFeatureFlags.mobileRemoteControlEnabled)
                ? .presets
                : tab
        } else if settingsTab == .mobile && !UnpeelFeatureFlags.mobileRemoteControlEnabled {
            settingsTab = .presets
        }
        // The settings nav takes over the sidebar list area; drop the
        // main-pane library so Back always returns to the project tree.
        archivedProjectID = nil
        recentActivityVisible = false
        settingsVisible = true
    }

    func closeSettings() {
        settingsVisible = false
    }


    /// Open a folder picker and reuse-or-add it as a project. Reports the
    /// chosen path back to the caller; reports `nil` if the user cancels.
    func pickProjectFolder(completion: @escaping @MainActor (String?) -> Void) {
        guard selectedHostScope.isLocalMachine else {
            completion(nil)
            return
        }
        let panel = NSOpenPanel()
        panel.canChooseDirectories = true
        panel.canChooseFiles = false
        panel.allowsMultipleSelection = false
        panel.title = "Select project folder"
        panel.prompt = "Add Project"
        panel.begin { [weak self] response in
            Task { @MainActor in
                guard response == .OK, let url = panel.url else {
                    completion(nil)
                    return
                }
                // Open the freshly added project (its launcher) by default.
                self?.openLauncher(forFolder: url.path)
                completion(url.path)
            }
        }
    }

    /// `showScanning` blanks the current report so the UI shows the scanning
    /// state; pass false for background refreshes (e.g. after an install
    /// finishes) so the Agent CLI Tools list doesn't flash empty.
    func refreshToolAvailability(
        showScanning: Bool = true,
        completion: (@MainActor (ToolScanReport) -> Void)? = nil
    ) {
        if showScanning { setupToolReport = nil }
        toolScanInProgress = true
        let started = Date()
        toolAvailability.scan { [weak self] report in
            Task { @MainActor in
                guard let self else { return }
                self.setupToolReport = report
                self.seedPresetPreferencesFromUsage(report)
                self.rescan()
                completion?(report)
                // Hold the scanning state briefly on fast scans so the
                // Rescan button's feedback is actually visible.
                let remaining = 0.5 - Date().timeIntervalSince(started)
                if remaining > 0 {
                    try? await Task.sleep(for: .seconds(remaining))
                }
                self.toolScanInProgress = false
            }
        }
    }

    /// Run a missing CLI's official install one-liner in the user's login
    /// shell (so npm/brew/curl resolve the way they do in their terminal),
    /// then rescan the PATH so the row moves into the installed list.
    func installTool(_ tool: SetupTool) {
        guard let command = tool.installCommand,
              !toolInstallsInProgress.contains(tool) else { return }
        toolInstallErrors[tool] = nil
        toolInstallsInProgress.insert(tool)
        DispatchQueue.global(qos: .userInitiated).async {
            let shell = ProcessInfo.processInfo.environment["SHELL"] ?? "/bin/zsh"
            let process = Process()
            process.executableURL = URL(fileURLWithPath: shell)
            process.arguments = ["-l", "-c", command]
            let pipe = Pipe()
            process.standardOutput = pipe
            process.standardError = pipe
            process.standardInput = FileHandle.nullDevice
            var failure: String?
            do {
                try process.run()
                let data = pipe.fileHandleForReading.readDataToEndOfFile()
                process.waitUntilExit()
                if process.terminationStatus != 0 {
                    let output = String(data: data, encoding: .utf8) ?? ""
                    NSLog("Unpeel install %@ failed (exit %d): %@",
                          tool.commandName, process.terminationStatus,
                          String(output.suffix(2000)))
                    // Keep the tail of the real output for the row's hover
                    // tooltip; npm's final "A complete log of this run can be
                    // found in …" pointer is noise, and the actual cause sits
                    // just above it.
                    let lines = output
                        .split(whereSeparator: \.isNewline)
                        .map { $0.trimmingCharacters(in: .whitespaces) }
                        .filter { !$0.isEmpty && !$0.contains("A complete log of this run") }
                    let tail = lines.suffix(5).joined(separator: "\n")
                    failure = tail.isEmpty
                        ? "Install failed (exit \(process.terminationStatus))"
                        : tail
                }
            } catch {
                failure = error.localizedDescription
            }
            Task { @MainActor [weak self] in
                guard let self else { return }
                self.toolInstallsInProgress.remove(tool)
                if let failure {
                    self.toolInstallErrors[tool] = failure
                } else {
                    self.refreshToolAvailability(showScanning: false) { report in
                        // An installer can exit 0 without putting the binary
                        // on the PATH (e.g. the package renamed its bin);
                        // silently restoring the Install button reads as
                        // "nothing happened".
                        if report.status(for: tool)?.installed != true {
                            self.toolInstallErrors[tool] =
                                "Installed, but \(tool.commandName) was not found on your PATH"
                        }
                    }
                }
            }
        }
    }

    /// First-run only (runs off the startup PATH scan): seed the flat preset
    /// order and favorites from detected session-store usage, so an existing
    /// user's most-used CLIs lead the quick strip and the Presets panel.
    /// Never touches an explicit saved order (the absent-`presetOrder`-key
    /// guard also makes it one-shot, so stars and drags stick), and leaves
    /// fresh machines (no usage anywhere) on the app-state order with the
    /// claude/codex default favorites.
    private func seedPresetPreferencesFromUsage(_ report: ToolScanReport) {
        guard !setupCompleted,
              AppDefaults.shared.object(forKey: Self.nativePresetOrderKey) == nil
        else { return }
        guard report.installedStatuses.contains(where: { $0.usage.hasAny }) else { return }
        let orderedCLIs = report.usageOrderedInstalledTools
        let cliRank = Dictionary(
            uniqueKeysWithValues: orderedCLIs.enumerated().map { ($1, $0) }
        )
        // Presets sorted by their CLI's usage rank (unranked CLIs, then custom
        // commands, keep their app-state order at the end).
        let ordered = mergedPresets.enumerated()
            .sorted { lhs, rhs in
                let lhsRank = SetupTool.detect(in: lhs.element.command).flatMap { cliRank[$0] } ?? Int.max
                let rhsRank = SetupTool.detect(in: rhs.element.command).flatMap { cliRank[$0] } ?? Int.max
                if lhsRank != rhsRank { return lhsRank < rhsRank }
                return lhs.offset < rhs.offset
            }
            .map(\.element)
        let seededOrder = ordered.map(\.id)
        applyPresetOrder(seededOrder)
        if presetsInSharedFile {
            // Stamp the legacy key too: it is this seeding's one-shot guard,
            // and an older build sharing the defaults starts from the same
            // order the file now has.
            presetOrder = seededOrder
            savePresetOrder()
        }

        // Favorites = the top 3 actually-used CLIs' leading presets. Unstar
        // the builtin claude/codex defaults when they didn't make the cut, so
        // the quick strip opens with what this user really reaches for.
        let top = Set(orderedCLIs.filter { report.status(for: $0)?.usage.hasAny == true }.prefix(3))
        guard !top.isEmpty else { return }
        var seenCLIs = Set<SetupTool>()
        for preset in ordered {
            guard let cli = SetupTool.detect(in: preset.command) else { continue }
            let want = top.contains(cli) && seenCLIs.insert(cli).inserted
            if preset.quickLaunch != want {
                updatePreset(id: preset.id, quickLaunch: want)
            }
        }
    }

    // MARK: - Appearance (Settings → Appearance)

    /// User picked a mode in the native Appearance panel: persist the
    /// overlay (it wins over app-state.json from now on) and re-apply.
    func setThemePreference(_ preference: ThemePreference) {
        AppDefaults.shared.set(preference.rawValue, forKey: Self.nativeThemeKey)
        guard preference != themePreference else { return }
        themePreference = preference
        applyAppAppearance()
    }

    private func nativeThemeOverride() -> ThemePreference? {
        AppDefaults.shared.string(forKey: Self.nativeThemeKey)
            .flatMap(ThemePreference.init(rawValue:))
    }

    /// Decision 4 (workspace-scope-and-pairing.md): the DEFAULT workspace's
    /// appearance is the inherited baseline. Its overlay lives in
    /// `.standard` — the bundle-id domain every instance shares — so a
    /// workspace with no setting of its own falls back to it. Nil on the
    /// default instance itself (its own overlay IS the baseline).
    static func inheritedDefaultTheme() -> ThemePreference? {
        guard !UnpeelWorkspaceContext.isDefaultInstance else { return nil }
        return UserDefaults.standard.string(forKey: nativeThemeKey)
            .flatMap(ThemePreference.init(rawValue:))
    }

    /// User picked an App color: persist, mirror into the Theme wash, and
    /// repaint — the published change re-renders store-observing SwiftUI, the
    /// notification restyles live Ghostty panes (SurfaceCache).
    func setAppTint(_ tint: AppTint) {
        AppDefaults.shared.set(tint.rawValue, forKey: Self.nativeAppTintKey)
        announceStateChange("native-overlay")
        guard tint != appTint else { return }
        applyAppTint(tint)
    }

    private func applyAppTint(_ tint: AppTint) {
        appTint = tint
        // Eased crossfade (hue arc + strength ramp) instead of a snap; the
        // completion republish lets store-observing views (row text, CTAs)
        // re-resolve at the final values — they render once at the start of
        // the fade and would otherwise keep a mid-animation wash.
        AppTintAnimator.shared.animate(toHue: tint.hue) { [weak self] in
            self?.objectWillChange.send()
        }
    }

    /// The saved tint, readable without a store instance (the /mobile
    /// bootstrap path advertises it to paired phones).
    nonisolated static func savedAppTint() -> AppTint {
        AppDefaults.shared.string(forKey: nativeAppTintKey)
            .flatMap(AppTint.init(rawValue:)) ?? .none
    }

    /// A peer instance changed THIS workspace's stored App color (its
    /// per-line color picker wrote our suite, then pinged `/reload-appearance`).
    /// Re-read and apply it so a running workspace recolors live, not just on
    /// its next launch.
    func reloadAppTintFromDisk() {
        let tint = Self.savedAppTint()
        // The sidebar picker's rows and footer dots re-read on this.
        NotificationCenter.default.post(name: .unpeelWorkspaceTintChanged, object: nil)
        // A peer can also change this workspace's saved MODE and
        // transparency (the scoped Appearance editor writes our suite the
        // same way the color pickers do) — re-resolve them alongside the
        // tint.
        let theme = nativeThemeOverride() ?? themePreference
        if theme != themePreference {
            themePreference = theme
            applyAppAppearance()
        }
        TransparencyModel.shared.reloadFromDefaults()
        // Peers also edit this workspace's Notifications/Experimental knobs
        // (and their reverts) through the same suite + ping — re-resolve
        // without materializing inherited values as own overrides.
        let resolvedMenuAttention = Self.resolveMenuAttentionDetection()
        if resolvedMenuAttention != menuAttentionDetectionEnabled {
            suppressMenuAttentionPersistence = true
            menuAttentionDetectionEnabled = resolvedMenuAttention
            suppressMenuAttentionPersistence = false
        }
        let resolvedFlags = Set(
            ExperimentalFeature.all
                .filter { UnpeelFeatureFlags.isEnabled($0) }
                .map(\.key)
        )
        if resolvedFlags != enabledExperimentalKeys {
            enabledExperimentalKeys = resolvedFlags
        }
        guard tint != appTint else { return }
        applyAppTint(tint)
        // If we are viewing our own Local scope, the chrome follows this too.
        if selectedHostScope == .local { applyScopeTint() }
    }

    /// The chrome tint follows the SELECTED workspace, not just this instance:
    /// scoping to another workspace repaints to its color and returning to
    /// Local restores ours. Display-only — never persists `appTint`.
    func applyScopeTint() {
        // 0.5s settle: a swipe commit's wash is deliberately partial at the
        // commit point (scrubWash trails the fingers at ~60% blend), so the
        // remaining color arrival is felt here, AFTER the switch, not
        // during the drag.
        AppTintAnimator.shared.animate(
            toHue: effectiveScopeTintHue(), duration: 0.5
        ) { [weak self] in
            self?.objectWillChange.send()
        }
    }

    private func effectiveScopeTintHue() -> Double? {
        switch selectedHostScope {
        case .local:
            return appTint.hue
        case .localWorkspace(let home, _):
            // The scoped workspace's own stored color. (home matches the
            // launcher's UNPEEL_HOME on this machine, so the suite hash lines
            // up with what its per-line picker wrote.)
            return Self.workspaceTint(home: home).hue
        case .remote(let hostID):
            // New Hosts carry the stable tint id with the rest of Appearance;
            // hostTintHue keeps compatibility with the earlier presentation
            // field. Until either lands, use the Controller-local label color.
            if let raw = remoteHostRuntime.snapshot?.workspaceSettings?
                .appearanceSettings?.appTint,
               let tint = AppTint(rawValue: raw) {
                return tint.hue
            }
            if let hue = remoteHostRuntime.snapshot?.hostTintHue { return hue }
            return RemoteWorkspaceTint.get(hostID).hue
        }
    }

    /// A local workspace's stored App color, read from its own defaults suite.
    static func workspaceTint(home: String) -> AppTint {
        AppDefaults.suite(forUnpeelHome: home)
            .string(forKey: nativeAppTintKey)
            .flatMap(AppTint.init(rawValue:)) ?? .none
    }

    /// User picked a default editor natively: persist the overlay (it wins
    /// over app-state.json's `code_editor`) and apply immediately so the
    /// "Open in editor" button and project menu pick it up.
    func setCodeEditor(_ editor: String) {
        AppDefaults.shared.set(editor, forKey: Self.nativeCodeEditorKey)
        guard editor != codeEditor else { return }
        codeEditor = editor
    }

    private func nativeCodeEditorOverride() -> String? {
        AppDefaults.shared.string(forKey: Self.nativeCodeEditorKey)
    }

    /// The selected editor id, readable without a store instance (cmd-click
    /// file opening runs from the terminal pane). Mirrors the overlay used by
    /// `nativeCodeEditorOverride`; defaults to VS Code like `codeEditor`.
    nonisolated static func preferredCodeEditor() -> String {
        AppDefaults.shared.string(forKey: nativeCodeEditorKey) ?? "code"
    }

    // MARK: - Advanced session cleanup

    private static func normalizedAutoStopArchiveMinutes(_ minutes: Int) -> Int {
        autoStopArchiveMinuteOptions.contains(minutes) ? minutes : 0
    }

    /// Resolve the shared file value: absent key = on at the default cutoff
    /// (opt-out feature); an explicit value — including 0 = Never — wins.
    static func resolvedAutoStopArchiveMinutes(_ stateFile: AppStateFile?) -> Int {
        guard let raw = stateFile?.autoStopArchiveMinutes else {
            return defaultAutoStopArchiveMinutes
        }
        return normalizedAutoStopArchiveMinutes(raw)
    }

    /// One-time fold of the legacy UserDefaults auto-stop minutes into
    /// `auto_stop_archive_minutes` in app-state.json, so the app and the TUI
    /// share a single knob. A key already present in the file wins (already
    /// folded, or written by a peer); an explicit legacy value — including
    /// 0 = Never — must survive the move. The legacy keys stay in the
    /// defaults suite (older builds sharing it keep working) but are never
    /// read again.
    private static func migrateAutoStopArchiveSetting() {
        guard let legacy = AppDefaults.shared
            .object(forKey: legacyAutoSessionStopMinutesKey) as? Int
        else { return }
        _ = PresetStateFile.edit { object in
            if object["auto_stop_archive_minutes"] == nil {
                object["auto_stop_archive_minutes"] = normalizedAutoStopArchiveMinutes(legacy)
            }
        }
    }

    /// Settings ▸ Appearance "Session titles". The knob lives in
    /// app-state.json so the TUI and every running session host see the
    /// change immediately (hosts re-read it per title event, no restart).
    func setSessionTitleMode(_ mode: SessionTitleMode) {
        _ = editPresetStateAnnouncing { object in
            object["session_title_mode"] = mode.rawValue
        }
        guard mode != sessionTitleMode else { return }
        sessionTitleMode = mode
    }

    func setAutoStopArchiveMinutes(_ minutes: Int) {
        let normalized = Self.normalizedAutoStopArchiveMinutes(minutes)
        // Always store explicitly — absent means "default on", so Never (0)
        // must be a written value, not a removed key.
        _ = editPresetStateAnnouncing { object in
            object["auto_stop_archive_minutes"] = normalized
        }
        guard normalized != autoStopArchiveMinutes else { return }
        autoStopArchiveMinutes = normalized
    }

    private static func normalizedSidebarStoppedLimit(_ limit: Int) -> Int {
        // Junk never silently *files* extra rows — it reads as the default
        // window (unlike auto-stop, where junk reads as off).
        sidebarStoppedLimitOptions.contains(limit) ? limit : defaultSidebarStoppedLimit
    }

    /// Resolve the shared file value: absent key = the default window; an
    /// explicit value — including 0 = None — wins.
    static func resolvedSidebarStoppedLimit(_ stateFile: AppStateFile?) -> Int {
        guard let raw = stateFile?.sidebarStoppedLimit else {
            return defaultSidebarStoppedLimit
        }
        return normalizedSidebarStoppedLimit(raw)
    }

    static func sidebarStoppedLimitLabel(for limit: Int) -> String {
        limit == 0 ? "None" : "\(limit)"
    }

    func setSidebarStoppedLimit(_ limit: Int) {
        let normalized = Self.normalizedSidebarStoppedLimit(limit)
        // Always store explicitly — absent means "default", so None (0)
        // must be a written value, not a removed key.
        _ = editPresetStateAnnouncing { object in
            object["sidebar_stopped_limit"] = normalized
        }
        guard normalized != sidebarVisibleSessionLimit else { return }
        sidebarVisibleSessionLimit = normalized
    }

    // The idle-session auto-stop-and-archive sweep is Host-owned
    // (crates/unpeel-serve/src/auto_archive.rs): the worker reads
    // `auto_stop_archive_minutes` from app-state.json on every sweep and
    // archives; the app only edits that setting (Settings ▸ Advanced) and
    // reflects the result on rescan. No app code path archives on its own.

    /// The mode the WINDOW should render in: light/dark follows the SELECTED
    /// workspace exactly like the chrome tint does. Scoping into another
    /// local workspace adopts its saved mode (its overlay, then its
    /// app-state theme, then system). A true remote Host supplies its
    /// Controller presentation additively in `workspaceSettings`.
    private func scopeThemePreference() -> ThemePreference {
        switch selectedHostScope {
        case .local:
            return themePreference
        case .localWorkspace(let home, _):
            return Self.workspaceThemePreference(home: home)
        case .remote:
            return remoteHostRuntime.snapshot?.workspaceSettings?
                .appearanceSettings
                .flatMap { ThemePreference(rawValue: $0.theme) }
                ?? themePreference
        }
    }

    /// A local workspace's saved mode, resolved the way its own instance
    /// would: defaults-suite overlay, then its app-state theme (state.rs
    /// `theme`, plain system/light/dark strings), then system.
    static func workspaceThemePreference(home: String) -> ThemePreference {
        let suite = AppDefaults.suite(forUnpeelHome: home)
        if let raw = suite.string(forKey: nativeThemeKey),
           let preference = ThemePreference(rawValue: raw) {
            return preference
        }
        let statePath = URL(fileURLWithPath: home, isDirectory: true)
            .appendingPathComponent("app-state.json")
        if let data = try? Data(contentsOf: statePath),
           let json = try? JSONSerialization.jsonObject(with: data) as? [String: Any],
           let raw = json["theme"] as? String,
           let preference = ThemePreference(rawValue: raw) {
            return preference
        }
        // Decision 4: no setting of its own — inherit the default
        // workspace's baseline from the shared `.standard` domain.
        if let raw = UserDefaults.standard.string(forKey: nativeThemeKey),
           let preference = ThemePreference(rawValue: raw) {
            return preference
        }
        return .system
    }

    /// Re-resolve the window's appearance from the current scope — for
    /// callers that just changed the SCOPED workspace's saved mode from this
    /// window (the scoped Appearance editor).
    func refreshScopeAppearance() {
        applyAppAppearance()
        applyScopeTransparency()
    }

    /// A remote Host's transparency is ephemeral presentation on this
    /// Controller. It must never become this local workspace's own saved
    /// preference; returning to Local restores the untouched defaults.
    private func applyScopeTransparency() {
        if case .remote = selectedHostScope,
           let appearance = remoteHostRuntime.snapshot?.workspaceSettings?
               .appearanceSettings {
            TransparencyModel.shared.applyScopedPresentation(
                background: appearance.backgroundOpacity,
                surface: appearance.surfaceOpacity,
                backgroundTone: appearance.backgroundTone,
                surfaceTone: appearance.surfaceTone
            )
        } else {
            // Local-workspace scope keeps the existing rule: transparency
            // renders in that workspace's own windows, while this Controller
            // keeps its own backdrop.
            TransparencyModel.shared.reloadFromDefaults()
        }
    }

    /// Decision 4's revert on THIS workspace instance: drop its own mode +
    /// transparency overrides so it inherits the default workspace's
    /// baseline, and re-resolve everything live. Color stays.
    func revertAppearanceToInheritedBaseline() {
        AppDefaults.shared.removeObject(forKey: Self.nativeThemeKey)
        TransparencyModel.clearSavedValues(in: AppDefaults.shared)
        TransparencyModel.shared.reloadFromDefaults()
        // Own suite (just cleared) → own app-state → inherited baseline →
        // system: the same chain the scoped reader resolves.
        let theme = Self.workspaceThemePreference(home: LaunchConfig.unpeelDir.path)
        if theme != themePreference {
            themePreference = theme
        }
        applyAppAppearance()
    }

    /// NSApp-level override (nil = follow macOS) so the window chrome,
    /// SwiftUI dynamic colors, vibrancy and the Ghostty surfaces all
    /// resolve from one appearance.
    private func applyAppAppearance() {
        NSApp.appearance = scopeThemePreference().nsAppearance
        writeAppAppearanceFile()
    }

    private func currentAppDarkMode() -> Bool {
        switch scopeThemePreference() {
        case .dark:
            return true
        case .light:
            return false
        case .system:
            return NSApp.effectiveAppearance.bestMatch(from: [.aqua, .darkAqua]) != .aqua
        }
    }

    private func writeAppAppearanceFile() {
        let value: String
        switch scopeThemePreference() {
        case .system:
            value = "system\n"
        case .dark:
            value = "dark\n"
        case .light:
            value = "light\n"
        }
        let url = LaunchConfig.unpeelDir.appendingPathComponent("app-appearance")
        do {
            try FileManager.default.createDirectory(
                at: url.deletingLastPathComponent(),
                withIntermediateDirectories: true
            )
            try value.write(to: url, atomically: true, encoding: .utf8)
        } catch {
            NSLog("[UnpeelNative] failed to write app appearance file: \(error)")
        }
    }

    // MARK: - MCP security (Settings → Sessions MCP)
    //
    // Unlike every other native preference, this is NOT a UserDefaults overlay:
    // Unpeel Sessions MCP reads `mcp_orchestrators` only from app-state.json, so
    // a grant that lived anywhere else would have no effect. It is the one field
    // the native app writes back to the shared file — via a field-preserving
    // read-modify-write (mutateAppStateJSON) that never drops the many keys the
    // native decoder doesn't model.

    private static func commandCanHostSessionsMCP(_ command: String?) -> Bool {
        SetupTool.detect(in: command ?? "")?
            .metadata?.capabilities.contains(.mcpSessions) == true
    }

    /// Write the legacy access-override map back to app-state.json as
    /// `{ role, reach }` objects. Access is no longer configured per session
    /// (reads are open and writes use a separate approval policy), so nothing
    /// adds new entries — but restart/prune still round-trip any legacy grants
    /// left in `app-state.json` so an old file keeps decoding cleanly.
    private func persistMcpGrants() {
        let snapshot = mcpOrchestrators.mapValues { grant in
            ["role": grant.role.rawValue, "reach": grant.reach.rawValue]
        }
        mutateAppStateJSON { root in
            root["mcp_orchestrators"] = snapshot
        }
    }

    /// Write the user-approved pairs to `mcp_write_approvals`
    /// in app-state.json so the Sessions MCP host sees them per write.
    private func persistMcpWriteApprovals() {
        let snapshot = mcpWriteApprovals
        mutateAppStateJSON { root in
            root["mcp_write_approvals"] = snapshot
        }
    }

    private func persistMcpAppOpenApprovals() {
        let snapshot = mcpAppOpenApprovals
        mutateAppStateJSON { root in
            root["mcp_app_open_approvals"] = snapshot
        }
    }

    func approveMcpAppOpen(caller: String, appID: String) {
        var apps = mcpAppOpenApprovals[caller] ?? []
        guard !apps.contains(appID) else { return }
        apps.append(appID)
        mcpAppOpenApprovals[caller] = apps
        persistMcpAppOpenApprovals()
    }

    func revokeMcpAppOpenApproval(caller: String, appID: String) {
        guard var apps = mcpAppOpenApprovals[caller], apps.contains(appID) else { return }
        apps.removeAll { $0 == appID }
        if apps.isEmpty {
            mcpAppOpenApprovals.removeValue(forKey: caller)
        } else {
            mcpAppOpenApprovals[caller] = apps
        }
        persistMcpAppOpenApprovals()
    }

    private func pruneMcpAppOpenApprovals(forRemovedSession sessionID: String) {
        guard mcpAppOpenApprovals.removeValue(forKey: sessionID) != nil else { return }
        persistMcpAppOpenApprovals()
    }

    private func carryMcpAppOpenApprovals(
        snapshot: [String: [String]], from oldID: String, to newID: String
    ) {
        guard let apps = snapshot[oldID], !apps.isEmpty else { return }
        var carried = mcpAppOpenApprovals[newID] ?? []
        for appID in apps where !carried.contains(appID) {
            carried.append(appID)
        }
        mcpAppOpenApprovals[newID] = carried
        persistMcpAppOpenApprovals()
    }

    /// Set the app-wide inter-session write policy (Settings ▸ Sessions use).
    /// Applied live: the host re-reads it per tool call.
    func setMcpNonChildWriteAccess(_ policy: McpNonChildWriteAccess) {
        guard policy != mcpNonChildWriteAccess else { return }
        mcpNonChildWriteAccess = policy
        mutateAppStateJSON { root in
            root["mcp_nonchild_write_access"] = policy.rawValue
        }
    }

    /// Remember the user's "Allow" answer for a caller→target write pair. The
    /// approval is directional and per pair; it lives until either session is
    /// removed (pruneNativeState) and follows a restart's new session id.
    func approveMcpWrite(caller: String, target: String) {
        var targets = mcpWriteApprovals[caller] ?? []
        guard !targets.contains(target) else { return }
        targets.append(target)
        mcpWriteApprovals[caller] = targets
        persistMcpWriteApprovals()
    }

    /// Revoke one remembered pair (Settings ▸ Sessions MCP ▸ approved list).
    /// The next write from `caller` to `target` asks again.
    func revokeMcpWriteApproval(caller: String, target: String) {
        guard var targets = mcpWriteApprovals[caller],
              targets.contains(target) else { return }
        targets.removeAll { $0 == target }
        if targets.isEmpty {
            mcpWriteApprovals.removeValue(forKey: caller)
        } else {
            mcpWriteApprovals[caller] = targets
        }
        persistMcpWriteApprovals()
    }

    /// Drop every approval involving a removed session (as writer or target).
    private func pruneMcpWriteApprovals(forRemovedSession sessionID: String) {
        var pruned = mcpWriteApprovals
        pruned.removeValue(forKey: sessionID)
        pruned = pruned.compactMapValues { targets in
            let kept = targets.filter { $0 != sessionID }
            return kept.isEmpty ? nil : kept
        }
        guard pruned != mcpWriteApprovals else { return }
        mcpWriteApprovals = pruned
        persistMcpWriteApprovals()
    }

    /// A restart mints a new session id; keep remembered approvals alive by
    /// re-adding, under the new id, every pair the old id appeared in (as
    /// writer or target). The snapshot is captured BEFORE pruneNativeState
    /// drops the old id's entries — same read-before-prune discipline as the
    /// carried access grant and provider conversation id.
    private func carryMcpWriteApprovals(
        snapshot: [String: [String]], from oldID: String, to newID: String
    ) {
        var changed = false
        // Pairs where the restarted session was the approved writer.
        for target in snapshot[oldID] ?? [] where target != newID {
            var targets = mcpWriteApprovals[newID] ?? []
            if !targets.contains(target) {
                targets.append(target)
                mcpWriteApprovals[newID] = targets
                changed = true
            }
        }
        // Pairs where it was the approved target.
        for (caller, targets) in snapshot
        where caller != oldID && caller != newID && targets.contains(oldID) {
            var kept = mcpWriteApprovals[caller] ?? []
            if !kept.contains(newID) {
                kept.append(newID)
                mcpWriteApprovals[caller] = kept
                changed = true
            }
        }
        if changed {
            persistMcpWriteApprovals()
        }
    }

    // MARK: - Computer MCP access (Settings → Computer)
    //
    // Same persistence contract as browser access: the unified MCP server
    // reads `computer_default_access` and `computer_approvals` from
    // app-state.json per call, so every change here applies live.

    /// Set the app-wide Computer access mode. Off applies live through the
    /// per-call gate; enabling reaches existing sessions at their next
    /// natural restart (domain advertising is launch-time). The engine
    /// daemon follows the mode: it runs exactly while access isn't Off.
    func setDefaultComputerAccess(_ access: ComputerAccess) {
        guard access != computerDefaultAccess else { return }
        computerDefaultAccess = access
        let value = access.rawValue
        mutateAppStateJSON { root in
            root["computer_default_access"] = value
        }
        ComputerEngineManager.shared.sync()
        rescan()
    }

    private func persistComputerApprovals() {
        let snapshot = computerApprovals
        mutateAppStateJSON { root in
            root["computer_approvals"] = snapshot
        }
    }

    /// Remember the user's "Allow" answer for one session (Ask mode). Lives
    /// until the session is removed; follows a restart's new session id.
    func approveComputerAccess(sessionID: String) {
        guard !computerApprovals.contains(sessionID) else { return }
        computerApprovals.append(sessionID)
        persistComputerApprovals()
    }

    /// Revoke one remembered approval (Settings ▸ Computer). The session's
    /// next computer action asks again.
    func revokeComputerApproval(sessionID: String) {
        guard computerApprovals.contains(sessionID) else { return }
        computerApprovals.removeAll { $0 == sessionID }
        persistComputerApprovals()
    }

    private func pruneComputerApprovals(forRemovedSession sessionID: String) {
        guard computerApprovals.contains(sessionID) else { return }
        computerApprovals.removeAll { $0 == sessionID }
        persistComputerApprovals()
    }

    /// A restart mints a new session id; keep the remembered approval alive.
    /// `approved` is captured BEFORE pruneNativeState drops the old id — the
    /// read-before-prune discipline every carried per-session fact uses.
    private func carryComputerApproval(approved: Bool, to newID: String) {
        guard approved, !computerApprovals.contains(newID) else { return }
        computerApprovals.append(newID)
        persistComputerApprovals()
    }

    // MARK: - Unpeel Link profile (Settings ▸ Remote ▸ Unpeel Link)
    //
    // The TUI edits the same keys, so these go through the flocked shared-file
    // editor (a concurrent TUI edit is never lost) and announce, so the other
    // frontend repaints at once.

    /// Save the presence nickname (`profile_display_name`).
    func setProfileDisplayName(_ name: String) {
        let trimmed = name.trimmingCharacters(in: .whitespacesAndNewlines)
        guard trimmed != profileDisplayName else { return }
        profileDisplayName = trimmed
        _ = editPresetStateAnnouncing { root in
            root["profile_display_name"] = trimmed
        }
    }

    /// Save the presence avatar (`profile_avatar`, an emoji from the shared
    /// picker set — `LinkLicenseSections.avatarChoices`, the TUI's
    /// `LINK_AVATARS`).
    func setProfileAvatar(_ avatar: String) {
        guard avatar != profileAvatar else { return }
        profileAvatar = avatar
        _ = editPresetStateAnnouncing { root in
            root["profile_avatar"] = avatar
        }
    }

    /// Toggle whether sessions may create worktrees (Settings ▸ Sessions
    /// use). Applied live: the host re-reads the flag per tool call.
    func setMcpWorktreeAccess(_ enabled: Bool) {
        guard enabled != mcpWorktreeAccess else { return }
        mcpWorktreeAccess = enabled
        mutateAppStateJSON { root in
            root["mcp_worktree_access"] = enabled
        }
    }

    /// Toggle Browser MCP's default screenshot destination. Applied live: the
    /// browser host re-reads the shared app-state key for every screenshot.
    func setMcpAutoAddBrowserScreenshots(_ enabled: Bool) {
        guard enabled != mcpAutoAddBrowserScreenshots else { return }
        mcpAutoAddBrowserScreenshots = enabled
        mutateAppStateJSON { root in
            root["mcp_auto_add_browser_screenshots"] = enabled
        }
    }

    private func persistBrowserApprovals() {
        let snapshot = browserApprovals
        mutateAppStateJSON { root in
            root["browser_approvals"] = snapshot
        }
    }

    /// Remember the user's "Allow" answer for one session (browser Ask mode).
    func approveBrowserAccess(sessionID: String) {
        guard !browserApprovals.contains(sessionID) else { return }
        browserApprovals.append(sessionID)
        persistBrowserApprovals()
    }

    /// Revoke one remembered browser approval (Settings ▸ Browser).
    func revokeBrowserApproval(sessionID: String) {
        guard browserApprovals.contains(sessionID) else { return }
        browserApprovals.removeAll { $0 == sessionID }
        persistBrowserApprovals()
    }

    private func pruneBrowserApprovals(forRemovedSession sessionID: String) {
        guard browserApprovals.contains(sessionID) else { return }
        browserApprovals.removeAll { $0 == sessionID }
        persistBrowserApprovals()
    }

    private func carryBrowserApproval(approved: Bool, to newID: String) {
        guard approved, !browserApprovals.contains(newID) else { return }
        browserApprovals.append(newID)
        persistBrowserApprovals()
    }

    // MARK: - Browser MCP access (Settings → Browser)
    //
    // Same persistence contract as the Sessions MCP default grant above: the
    // host's `__browser_mcp__` server reads `browser_default_access` only from
    // app-state.json (re-read per tool call), so the native app writes it
    // through the same field-preserving mutateAppStateJSON. Browser access is a
    // single app-wide on/off — there is no per-session override.

    /// Set the app-wide Browser Access and persist it. Browser access is a
    /// single global on/off — every capable session uses this. Applied live:
    /// the host re-reads `browser_default_access` per tool call.
    func setDefaultBrowserAccess(_ access: BrowserAccess) {
        guard access != browserDefaultAccess else { return }
        browserDefaultAccess = access
        persistBrowserDefaultAccess()
        rescan()
    }

    private func persistBrowserDefaultAccess() {
        let value = browserDefaultAccess.rawValue
        mutateAppStateJSON { root in
            root["browser_default_access"] = value
        }
    }

    /// Update the app-wide browser engine options and persist them. Applied
    /// live: the host re-reads `browser_settings` on every engine invocation.
    func updateBrowserSettings(_ mutate: (inout BrowserSettings) -> Void) {
        var updated = browserSettings
        mutate(&updated)
        guard updated != browserSettings else { return }
        browserSettings = updated
        let snapshot: [String: Any] = [
            "headed": updated.headed,
            "allowed_domains": updated.allowedDomains,
            "profile_mode": updated.profileMode,
            "executable_path": updated.executablePath,
            "show_cursor": updated.showCursor,
        ]
        mutateAppStateJSON { root in
            root["browser_settings"] = snapshot
        }
    }

    /// Update the app-wide transcript rendering options and persist them.
    /// Applied live: the host re-reads `transcript_settings` from app-state.json
    /// each time it builds a Markdown transcript (Copy Transcript / MCP
    /// `read_transcript`), so changes take effect on the next copy or read.
    func updateTranscriptSettings(_ mutate: (inout TranscriptSettings) -> Void) {
        var updated = transcriptSettings
        mutate(&updated)
        guard updated != transcriptSettings else { return }
        transcriptSettings = updated
        let snapshot: [String: Any] = [
            "include_user": updated.includeUser,
            "include_assistant": updated.includeAssistant,
            "include_reasoning": updated.includeReasoning,
            "include_tools": updated.includeTools,
            "include_file_changes": updated.includeFileChanges,
            "include_plan_updates": updated.includePlanUpdates,
            "include_session_info": updated.includeSessionInfo,
            "max_entries": updated.maxEntries,
        ]
        mutateAppStateJSON { root in
            root["transcript_settings"] = snapshot
        }
    }

    /// Delete the "kept per project" browsing data: the Unpeel-managed
    /// profiles (localStorage/cache) AND the engine's saved login state.
    /// A live engine daemon keeps its already-open browser state until that
    /// session's browser closes; new browsers start clean.
    func clearBrowserProfiles() {
        let dir = LaunchConfig.unpeelDir.appendingPathComponent("browser/profiles")
        try? FileManager.default.removeItem(at: dir)
        // Login *state* (cookies) lives in the engine's own store, not the
        // profile dir — the host saves it per project as
        // ~/.agent-browser/sessions/unpeel-proj-*.json[.enc]. Clearing must
        // cover both or "Clear" silently keeps every login. Always the real
        // home: the engine ignores UNPEEL_HOME.
        let engineSessions = FileManager.default.homeDirectoryForCurrentUser
            .appendingPathComponent(".agent-browser/sessions")
        if let files = try? FileManager.default.contentsOfDirectory(
            at: engineSessions, includingPropertiesForKeys: nil
        ) {
            for file in files where file.lastPathComponent.hasPrefix("unpeel-proj-") {
                try? FileManager.default.removeItem(at: file)
            }
        }
    }

    /// Effective MCP block state for a project, including explicit id-keyed
    /// blocks, legacy project flags, and inherited parent/worktree blocks.
    func projectMcpBlocked(_ projectID: String) -> Bool {
        var current = projectID
        for _ in 0..<16 {
            if mcpBlockedProjectIDs.contains(current) { return true }
            guard let project = projectsByID[current] else { return false }
            if project.mcpBlocked == true { return true }
            guard let parent = project.parentProjectID else { return false }
            current = parent
        }
        return false
    }

    /// Block or unblock a project from MCP by id (works for overlay-only and
    /// worktree projects, which never appear in app-state.json's `projects`).
    func setProjectMcpBlocked(_ projectID: String, blocked: Bool) {
        let affectedIDs = projectIDAndWorktreeDescendants(projectID)
        if blocked { mcpBlockedProjectIDs.formUnion(affectedIDs) }
        else { mcpBlockedProjectIDs.subtract(affectedIDs) }
        mutateAppStateJSON { root in
            var ids = (root["mcp_blocked_projects"] as? [String]) ?? []
            ids.removeAll { affectedIDs.contains($0) }
            if blocked {
                ids.append(contentsOf: affectedIDs.sorted())
            }
            root["mcp_blocked_projects"] = ids
        }
    }

    private func projectIDAndWorktreeDescendants(_ projectID: String) -> Set<String> {
        var result: Set<String> = [projectID]
        var stack = [projectID]
        while let current = stack.popLast() {
            let children = projectsByID.values.filter {
                $0.parentProjectID == current && $0.worktreeBranch != nil
            }
            for child in children where !result.contains(child.id) {
                result.insert(child.id)
                stack.append(child.id)
            }
        }
        return result
    }

    /// Read-modify-write app-state.json at the JSON-object level so unmodeled
    /// keys (presets, tags, saved_sessions, …) survive untouched. Writes
    /// atomically; seeds a minimal object if the file is absent or unreadable.
    /// Invalidates the decode cache so the next rescan re-reads the new value.
    private func mutateAppStateJSON(_ mutate: (inout [String: Any]) -> Void) {
        let url = LaunchConfig.appStateFile
        var root: [String: Any] = {
            guard let data = try? Data(contentsOf: url),
                  let object = try? JSONSerialization.jsonObject(with: data),
                  let dict = object as? [String: Any]
            else { return [:] }
            return dict
        }()
        Self.seedAppStateDefaults(&root)
        mutate(&root)
        guard let data = try? JSONSerialization.data(
            withJSONObject: root, options: [.prettyPrinted, .sortedKeys]
        ) else {
            NSLog("[UnpeelNative] failed to serialize app-state.json for MCP security write")
            return
        }
        do {
            // Ensure the directory exists, then write atomically.
            try FileManager.default.createDirectory(
                at: url.deletingLastPathComponent(), withIntermediateDirectories: true
            )
            try data.write(to: url, options: .atomic)
            appStateCache = nil
        } catch {
            NSLog("[UnpeelNative] failed to write app-state.json: \(error)")
        }
    }

    private static func seedAppStateDefaults(_ root: inout [String: Any]) {
        func assign(_ key: String, _ value: @autoclosure () -> Any) {
            if root[key] == nil {
                root[key] = value()
            }
        }

        assign("projects", [] as [Any])
        assign("active_project_id", NSNull())
        assign("workspaces", [["id": "personal", "name": "Personal"]])
        assign("active_workspace_id", "personal")
        assign("presets", builtinPresetJSON())
        assign("tags", [] as [Any])
        assign("active_tabs", [:] as [String: Any])
        assign("saved_sessions", [] as [Any])
        assign("pinned_sessions", [:] as [String: Any])
        assign("theme", "system")
        assign("color_scheme", "default")
        assign("code_editor", "code")
        assign("last_sessions", [:] as [String: Any])
        assign("setup_completed", false)
        assign("mcp_orchestrators", [:] as [String: Any])
        assign("mcp_default_access", ["role": "read", "reach": "project"])
        assign("mcp_nonchild_write_access", "ask")
        assign("mcp_write_approvals", [:] as [String: Any])
        assign("mcp_blocked_projects", [] as [Any])
        assign("browser_default_access", "on")
    }

    /// Browser access is a single app-wide switch (Settings ▸ Browser), and the
    /// Device MCP was removed entirely. Strip any legacy per-session
    /// `browser_access` / `device_access` maps (and the removed
    /// `device_default_access`) so a stale entry can't silently override the
    /// global default on the host's per-call gate. No-op once cleared.
    private func migrateAwayFromPerSessionMediaAccess() {
        guard let data = try? Data(contentsOf: LaunchConfig.appStateFile),
              let object = try? JSONSerialization.jsonObject(with: data),
              let dict = object as? [String: Any]
        else { return }
        let hasBrowser = (dict["browser_access"] as? [String: Any])?.isEmpty == false
        let hasDevice = (dict["device_access"] as? [String: Any])?.isEmpty == false
        let hasDeviceDefault = dict["device_default_access"] != nil
        guard hasBrowser || hasDevice || hasDeviceDefault else { return }
        mutateAppStateJSON { root in
            root.removeValue(forKey: "browser_access")
            root.removeValue(forKey: "device_access")
            root.removeValue(forKey: "device_default_access")
        }
    }

    private static func builtinPresetJSON() -> [[String: Any]] {
        Preset.builtinGlobalPresets.map { preset in
            [
                "id": preset.id,
                "label": preset.label,
                "command": preset.command,
                "project_id": NSNull(),
                "enabled": preset.enabled,
                "quick_launch": preset.quickLaunch,
            ]
        }
    }

    nonisolated static func shouldPublishDeferredStopEffects(
        observedGeneration: UInt64?,
        currentGeneration: UInt64?
    ) -> Bool {
        observedGeneration == currentGeneration
    }

    private struct RuntimeLaunchSnapshot {
        let generation: UInt64
        let launchedAt: Date?
    }

    private nonisolated static func runtimeLaunchSnapshotOnDisk(
        _ sessionID: String
    ) -> RuntimeLaunchSnapshot? {
        let url = LaunchConfig.appSessionsDir
            .appendingPathComponent(sessionID)
            .appendingPathComponent("manifest.json")
        guard let data = try? Data(contentsOf: url),
              let manifest = try? JSONDecoder().decode(HostedSessionManifest.self, from: data)
        else { return nil }
        return RuntimeLaunchSnapshot(
            generation: manifest.runtimeLaunchGeneration,
            launchedAt: manifest.runtimeLaunchedAt.map {
                Date(timeIntervalSince1970: TimeInterval($0) / 1_000)
            }
        )
    }

    /// Selection / focus changed: clear the now-observed session's unread
    /// and run the pending-unread reconciliation (App.svelte
    /// handleSelectSession:1264-1267 + the reconcile $effect:806-828).
    private func handleObservationChanged() {
        // ⌘ hints can't outlive app focus: the flagsChanged monitor never
        // sees the release once another app is frontmost.
        if !NSApp.isActive, commandHintsVisible {
            commandHintsVisible = false
        }
        // Selection cannot change any Session's lifecycle state. Persist
        // only if reconciliation actually changes unread membership; the
        // rescan path still requests a full status snapshot below.
        reconcileUnread(persistSessionStates: false)
        refreshTitlebarBranch()
    }

    private func reconcileUnread(persistSessionStates: Bool = true) {
        // Reconciliation only consults the previous observation and the
        // pending set. Building a status dictionary for every historical
        // Session made a simple tab switch scale with the whole workspace.
        var relevantSessionIDs = pendingUnreadSessions
        if let previousObservedSessionID {
            relevantSessionIDs.insert(previousObservedSessionID)
        }
        if let observedSessionID {
            relevantSessionIDs.insert(observedSessionID)
        }
        var states: [String: SessionStatus] = [:]
        states.reserveCapacity(relevantSessionIDs.count)
        for id in relevantSessionIDs {
            if let session = sessionsByID[id] {
                states[id] = session.status
            }
        }

        let unreadBeforeReconciliation = unreadSessionIDs
        let result = UnreadReconciliation.reconcile(
            pendingUnreadSessions: pendingUnreadSessions,
            sessionStates: states,
            completedSessionIDs: completedSessionIDs,
            previousObservedSessionID: previousObservedSessionID,
            currentObservedSessionID: observedSessionID
        )
        pendingUnreadSessions = result.pendingUnreadSessions
        for sessionID in result.unreadToClear { removeUnread(sessionID) }
        for sessionID in result.unreadToMark { markUnread(sessionID) }
        previousObservedSessionID = observedSessionID
    }

    private func markUnread(_ sessionID: String) {
        guard !unreadSessionIDs.contains(sessionID) else { return }
        unreadSessionIDs.insert(sessionID)
        // Unread blocks stay visible past the stopped-group window.
        invalidateSidebarLists()
    }

    /// Append one event to the persisted history feed (the Recent panel).
    /// Same-kind repeats per session collapse in ActivityLogStore, so a
    /// permission loop or back-to-back turn finishes bump one row.
    private func logActivity(
        _ kind: ActivityLogEntry.Kind,
        sessionID: String,
        message: String? = nil
    ) {
        guard let session = sessionsByID[sessionID]
            ?? pendingSessions[sessionID]
            ?? restartPlaceholders[sessionID]
        else { return }
        let title = session.label.trimmingCharacters(in: .whitespacesAndNewlines)
        activityLog.append(ActivityLogEntry(
            id: UUID().uuidString,
            sessionID: sessionID,
            kind: kind,
            at: UInt64(Date().timeIntervalSince1970 * 1000),
            title: title.isEmpty ? "Untitled session" : title,
            command: session.command,
            projectID: session.projectID,
            projectName: projectsByID[session.projectID] != nil
                ? activityProjectName(session.projectID) : "",
            message: message
        ))
        activityLogEntries = activityLog.entries
    }

    /// The push reasons, matched to the phone's notification copy + tap
    /// handling (`kind` in the APNs payload).
    enum SessionPushKind: String {
        case needsInput = "needs_input"
        case done
        case alert
        case approval
        case test
    }

    struct NotificationDeliveryPolicy: Equatable {
        let markUnread: Bool
        let sendDesktop: Bool
    }

    struct MenuPromptNotificationState: Equatable {
        let runtimeGeneration: UInt64
        let active: Bool
        /// True once either the menu edge or its matching PermissionRequest
        /// hook has emitted the needs-input notification.
        let notificationSent: Bool
    }

    struct MenuPromptNotificationDecision: Equatable {
        let state: MenuPromptNotificationState
        let sendNotification: Bool
    }

    /// Reduce one host-observed `menu_prompt_active` sample. The first app scan
    /// seeds state without alerting, while a session first discovered after
    /// startup may alert immediately if it is already active. Every later
    /// false -> true edge alerts once. A runtime generation change is also a
    /// re-arm because the Host resets this flag when it launches the new agent,
    /// even if native missed that short-lived false write.
    nonisolated static func menuPromptNotificationDecision(
        previous: MenuPromptNotificationState?,
        runtimeGeneration: UInt64,
        active: Bool,
        initialAppScan: Bool,
        detectionEnabled: Bool,
        dismissed: Bool,
        hookAlreadyNeedsInput: Bool
    ) -> MenuPromptNotificationDecision {
        guard let previous else {
            let send = active
                && !initialAppScan
                && detectionEnabled
                && !dismissed
                && !hookAlreadyNeedsInput
            return MenuPromptNotificationDecision(
                state: MenuPromptNotificationState(
                    runtimeGeneration: runtimeGeneration,
                    active: active,
                    // A PermissionRequest can arrive before native's first
                    // manifest sample. Remember that delivery so the menu and
                    // a repeated hook cannot emit the same semantic alert.
                    notificationSent: active && (send || hookAlreadyNeedsInput)
                ),
                sendNotification: send
            )
        }
        guard active else {
            return MenuPromptNotificationDecision(
                state: MenuPromptNotificationState(
                    runtimeGeneration: runtimeGeneration,
                    active: false,
                    notificationSent: false
                ),
                sendNotification: false
            )
        }

        let rose = previous.runtimeGeneration != runtimeGeneration || !previous.active
        guard rose else {
            return MenuPromptNotificationDecision(
                state: previous,
                sendNotification: false
            )
        }
        let send = detectionEnabled && !dismissed && !hookAlreadyNeedsInput
        return MenuPromptNotificationDecision(
            state: MenuPromptNotificationState(
                runtimeGeneration: runtimeGeneration,
                active: true,
                // A hook-owned attention state means the hook already emitted
                // this semantic alert. Otherwise record only an actual menu
                // delivery; a disabled menu detector must never suppress a
                // later authoritative PermissionRequest hook.
                notificationSent: send || hookAlreadyNeedsInput
            ),
            sendNotification: send
        )
    }

    /// Claim an authoritative PermissionRequest against the currently visible
    /// menu. If the menu edge already alerted, suppress only that duplicate.
    /// An initially-active menu has not alerted, so a later hook still sends.
    nonisolated static func permissionRequestNotificationDecision(
        previous: MenuPromptNotificationState?,
        runtimeGeneration: UInt64?
    ) -> (state: MenuPromptNotificationState?, sendNotification: Bool) {
        guard let previous,
              let runtimeGeneration,
              previous.runtimeGeneration == runtimeGeneration,
              previous.active
        else { return (previous, true) }
        guard !previous.notificationSent else { return (previous, false) }
        return (
            MenuPromptNotificationState(
                runtimeGeneration: previous.runtimeGeneration,
                active: true,
                notificationSent: true
            ),
            true
        )
    }

    /// Desktop observation and phone delivery are independent channels. A
    /// user looking at a session on this Mac does not imply that every paired
    /// phone is also being watched; Link still delivers to background phones.
    /// Any live Controller viewer suppresses the local unread/banner, while
    /// phone fan-out applies the precise per-device check below.
    nonisolated static func notificationDeliveryPolicy(
        macIsObserving: Bool,
        anyControllerIsViewing: Bool
    ) -> NotificationDeliveryPolicy {
        let observedAnywhere = macIsObserving || anyControllerIsViewing
        return NotificationDeliveryPolicy(
            markUnread: !observedAnywhere,
            sendDesktop: !observedAnywhere
        )
    }

    /// Deterministic Settings diagnostic. Unlike a lifecycle alert, an
    /// explicit test intentionally bypasses viewer suppression so it can prove
    /// the complete production TestFlight → Link → APNs path while the
    /// user has the settings screens open.
    /// Phone push for the two kinds the app still originates itself:
    /// approval prompts mirrored from the Host and the Settings test push.
    /// Lifecycle edges (needs input / done / alert) are Host-owned and reach
    /// the phone through the `notification.deliver` platform callback.
    private func dispatchSessionPush(
        sessionID: String,
        kind: SessionPushKind,
        titleOverride: String? = nil,
        bodyOverride: String? = nil
    ) {
        guard kind == .approval || kind == .test else { return }
        let rawTitle = (displaySessionsByID[sessionID] ?? sessionsByID[sessionID])?
            .label.trimmingCharacters(in: .whitespacesAndNewlines) ?? ""
        let customTitle = titleOverride?.trimmingCharacters(in: .whitespacesAndNewlines)
        let title = if let customTitle, !customTitle.isEmpty {
            customTitle
        } else {
            rawTitle.isEmpty ? "Unpeel session" : rawTitle
        }
        let body = bodyOverride
            ?? (kind == .approval ? "Waiting for your approval" : "Link notifications are working")
        dispatchPhonePush(
            sessionID: sessionID,
            title: title,
            body: body,
            kind: kind,
            suppressViewingTargets: true
        )
    }

    func sendTestPhoneNotification() {
        let sessionID = selectedSessionID
            ?? sessionsByID.values.first?.id
            ?? "unpeel-test"
        dispatchPhonePush(
            sessionID: sessionID,
            title: "Unpeel",
            body: "Link notifications are working",
            kind: .test,
            suppressViewingTargets: false
        )
    }

    private func dispatchPhonePush(
        sessionID: String,
        title: String,
        body: String,
        kind: SessionPushKind,
        suppressViewingTargets: Bool,
        suppressedDeviceIDs: Set<String> = []
    ) {
        // Phone push uses Link/APNs even when terminal traffic is currently
        // Direct or SSH. The Relay WebSocket does not need to be connected.
        let pairingStore = platformPairingStore
        let targets = pairingStore.pushTargets()
        guard !targets.isEmpty else { return }
        let eligibleTargets = targets.filter { target in
            !suppressedDeviceIDs.contains(target.deviceID)
                && (!suppressViewingTargets
                || !ViewerPresenceStore.shared.isDeviceViewing(
                    sessionID: sessionID,
                    deviceID: target.deviceID
                ))
        }
        guard !eligibleTargets.isEmpty else { return }
        Task {
            for target in eligibleTargets {
                let result = await RelayUplinkManager.shared.sendPush(
                    apnsToken: target.token,
                    environment: target.environment,
                    title: title,
                    body: body,
                    sessionID: sessionID,
                    kind: kind.rawValue,
                    macID: localHostID
                )
                // APNs says this token is dead — stop pushing to it.
                if let reason = result.reason,
                   reason == "BadDeviceToken" || reason == "Unregistered" {
                    pairingStore.clearPushToken(deviceID: target.deviceID)
                    self.refreshPairedControllers()
                }
            }
        }
    }

    /// Push "wants your approval" to paired phones when a new approval prompt
    /// is enqueued (MCPApprovalCenter). Always pushed — like PermissionRequest,
    /// the asking agent is blocked and times out in ~2 minutes — except to a
    /// phone actively viewing the caller session, which sees the in-app prompt
    /// on its next bootstrap poll within seconds.
    func notifyMcpApprovalRequested(_ approval: PendingMcpApproval) {
        let body: String
        switch approval.kind {
        case .write:
            let target = approval.targetSessionID.map(sessionDisplayName) ?? "another session"
            body = "Wants to type into “\(target)”"
        case .browser:
            body = "Wants to use a browser"
        case .computer:
            body = "Wants to control this Mac"
        case .appOpen:
            body = "Wants to open \(approval.targetAppName ?? approval.targetAppID ?? "an App")"
        }
        dispatchSessionPush(
            sessionID: approval.callerSessionID, kind: .approval, bodyOverride: body
        )
    }

    /// Whether showing a session must advance its shared read receipt. A
    /// receipt already at or beyond the latest real activity is a true no-op:
    /// rewriting it only wakes every frontend, while changing no unread
    /// result. Missing activity likewise remains covered by any receipt.
    nonisolated static func sharedReadReceiptNeedsRefresh(
        readAt: Int64?, settledAt: Int64?
    ) -> Bool {
        guard let readAt else { return true }
        return settledAt.map { $0 > readAt } ?? false
    }

    /// Lifecycle settles and App alerts independently create unread work.
    /// Keep their maximum as the receipt/recent timestamp without treating an
    /// Alert as a lifecycle transition.
    nonisolated static func latestUnreadActivityAt(
        lifecycleAt: Int64?, alertAt: Int64?
    ) -> Int64? {
        switch (lifecycleAt, alertAt) {
        case let (lifecycle?, alert?): max(lifecycle, alert)
        case let (lifecycle?, nil): lifecycle
        case let (nil, alert?): alert
        case (nil, nil): nil
        }
    }

    private static let readReceiptQueue = DispatchQueue(
        label: "com.unpeel.read-receipts",
        qos: .utility
    )

    private func removeUnread(_ sessionID: String) {
        // The visible state is entirely in memory, so clear it synchronously.
        // The shared receipt requires several filesystem reads (and sometimes
        // an atomic write); doing those in a sidebar click handler made even a
        // stopped Session wait on disk before SwiftUI could render selection.
        if unreadSessionIDs.remove(sessionID) != nil {
            invalidateSidebarLists()
        }

        // Always ensure a receipt covers the latest settle, even when this
        // app didn't think the row was unread: another frontend may still be
        // showing the dot. A serial utility queue preserves write ordering and
        // makes rapid A/B switching converge without blocking the main actor.
        let command = sessionsByID[sessionID]?.command ?? ""
        let alertAt = latestAlertAtMs(for: sessionID)
        let ownPort = hookServer?.port
        Self.readReceiptQueue.async {
            let readAt = (
                Self.readSharedMarker(sessionID, .read)?["read_at"] as? NSNumber
            )?.int64Value
            let settledAt = Self.latestUnreadActivityAt(
                lifecycleAt: Self.sessionLastRealActivityAtMs(
                    sessionID,
                    command: command
                ),
                alertAt: alertAt
            )
            if Self.sharedReadReceiptNeedsRefresh(readAt: readAt, settledAt: settledAt),
               Self.writeSharedMarker(
                   sessionID,
                   .read,
                   ["read_at": Int64(Date().timeIntervalSince1970 * 1000)]
               ) {
                Self.announceStateChange("session-markers", ownPort: ownPort)
            }
        }
    }

    // MARK: - Scanning

    /// The synchronous Swift scan is a startup/recovery seed only once Local
    /// is a Host client. Reapplying that frozen seed after the first complete
    /// Host projection can remove a just-created row, clear its selection,
    /// and then make the next Host snapshot add it back again.
    nonisolated static func shouldApplyLocalDiskProjection(
        localHostClientStarted: Bool,
        localHostProjectionReady: Bool
    ) -> Bool {
        !localHostClientStarted || !localHostProjectionReady
    }

    private var shouldApplyLocalDiskProjection: Bool {
        Self.shouldApplyLocalDiskProjection(
            localHostClientStarted: localHostClientStarted,
            localHostProjectionReady: localHostProjectionReady
        )
    }

    /// Synchronous rescan: collects the disk snapshot inline (on the main
    /// thread) and applies it. Direct callers (startup, user actions, tests)
    /// rely on the state being current when this returns. The recurring
    /// triggers — FSEvents, busy sweep, safety net — go through
    /// `scheduleRescan`, which collects on a background queue instead.
    func rescan() {
        guard !initialScanPending else { return }
        guard shouldApplyLocalDiskProjection else {
            // Explicit legacy callers use `rescan()` as their "show the write"
            // barrier. In client-only Local scope, ask the actual model owner
            // for a fresh snapshot instead of rebuilding from the stale seed.
            remoteHostRuntime.requestImmediateRefresh()
            // The shared preset file is still THIS app's own write target
            // (Add agent/App, star, reorder…) and feeds "Agents/Apps you can
            // add" through mergedPresets. Reload it here too: the worker
            // republishes the launch list, but never fed these lists, so a
            // just-added agent kept being offered (0.4.0 duplicate-Add bug).
            reloadSharedPresets()
            refreshInstalledApps()
            return
        }
        performRescan(snapshot: collectScanSnapshot())
        refreshInstalledApps()
    }

    /// Which preset lists a `rescan()` refreshes: the disk projection path
    /// rebuilds everything; the Host-service client path must still reload
    /// the shared preset file this app writes itself.
    enum PresetReloadPlan: Equatable { case fullDiskProjection, sharedPresetFileOnly }

    nonisolated static func presetReloadPlan(appliesLocalDiskProjection: Bool) -> PresetReloadPlan {
        appliesLocalDiskProjection ? .fullDiskProjection : .sharedPresetFileOnly
    }

    /// Reload the flat preset list from app-state.json with exactly the
    /// seeding rules `performRescan` applies, without touching sessions.
    private func reloadSharedPresets() {
        let stateFile = loadAppState()
        let globalPresets: [Preset]
        if let presets = stateFile?.presets {
            globalPresets = presets
        } else if stateFile == nil {
            globalPresets = Preset.builtinGlobalPresets
        } else {
            globalPresets = []
        }
        rebuildPresets(
            globalPresets: globalPresets,
            setupDone: stateFile?.setupCompleted ?? false,
            overlayMigrated: stateFile?.nativePresetOverlayMigrated ?? false,
            allowFold: stateFile != nil
                || !FileManager.default.fileExists(atPath: LaunchConfig.appStateFile.path)
        )
    }

    /// One-time repair (0.4.1) for the exact-duplicate presets the 0.4.0
    /// client-mode Add bug appended: collapse identical rows in the shared
    /// file once per instance, announcing like any other preset write.
    private static let duplicatePresetRepairKey = "unpeel.native.presetDuplicateRepair.1"

    func repairDuplicatePresetsOnce() {
        guard presetsInSharedFile,
              selectedHostScope == .local,
              !AppDefaults.shared.bool(forKey: Self.duplicatePresetRepairKey)
        else { return }
        AppDefaults.shared.set(true, forKey: Self.duplicatePresetRepairKey)
        var removed = 0
        editPresetStateAnnouncing { object in
            let result = PresetStateFile.collapseExactDuplicates(PresetStateFile.rawPresets(of: object))
            removed = result.removed
            if removed > 0 { object["presets"] = result.rows }
        }
        if removed > 0 {
            NSLog("[UnpeelNative] presets: collapsed %d exact duplicate row(s)", removed)
            reloadSharedPresets()
        }
    }

    private func collectScanSnapshot() -> ScanDiskSnapshot {
        scanCollector.collect(
            root: LaunchConfig.appSessionsDir.path,
            purged: purgedSessionDirs,
            purgedTTL: Self.purgedSessionDirTTL,
            now: Date()
        )
    }

    /// Background-collected rescan for the recurring triggers: the disk
    /// pass runs on `scanCollectQueue`, only the apply runs on main. At
    /// most one collection is in flight; a request that arrives mid-flight
    /// coalesces into one follow-up.
    private func rescanCollectingOffMain() {
        guard !initialScanPending else { return }
        guard shouldApplyLocalDiskProjection else { return }
        if scanCollectInFlight {
            scanRecollectQueued = true
            return
        }
        scanCollectInFlight = true
        let root = LaunchConfig.appSessionsDir.path
        let purged = purgedSessionDirs
        let ttl = Self.purgedSessionDirTTL
        let collector = scanCollector
        scanCollectQueue.async {
            let snapshot = collector.collect(
                root: root, purged: purged, purgedTTL: ttl, now: Date()
            )
            Task { @MainActor [weak self] in
                guard let self else { return }
                self.scanCollectInFlight = false
                self.performRescan(snapshot: snapshot)
                if self.scanRecollectQueued {
                    self.scanRecollectQueued = false
                    self.scheduleRescan(after: 0)
                }
            }
        }
    }

    private func performRescan(snapshot: ScanDiskSnapshot) {
        // A collection can start before the first Host bootstrap and finish
        // after it. Check again on apply so that in-flight fallback work can
        // never overwrite the authoritative projection.
        guard shouldApplyLocalDiskProjection else { return }
        // Defer while a menu is open: publishing store changes mid-track
        // rebuilds the open NSMenu and blinks its flyout submenus. The
        // initial scan is exempt (no menu can be open before first render,
        // and callers rely on hasCompletedScan flipping).
        if menuTrackingDepth > 0, hasCompletedScan {
            rescanDeferredForMenuTracking = true
            return
        }
        // Defer while the user is actively scrolling (wheel or momentum):
        // the scan's disk pass plus the sidebar rebuild it publishes block
        // the main queue for tens of milliseconds, and ghostty presents
        // every terminal frame via a main-queue hop — one rescan mid-scroll
        // is a visible frame drop. Bounded: a sustained scroll still gets a
        // rescan every few seconds so busy spinners expire and structural
        // changes (exits, new sessions) surface.
        let now = Date()
        if hasCompletedScan,
           now.timeIntervalSince(lastScrollWheelEventAt) < 0.3,
           now.timeIntervalSince(lastFullRescanAt) < 3.0 {
            scheduleRescan(after: 0.4)
            return
        }
        lastFullRescanAt = now
        // Folder colors are UserDefaults-backed but cross-frontend now: the
        // TUI's color menu writes the same key and pings the state bus, so
        // re-read here or a running app shows (and later saves) stale colors.
        let externalColors = Self.loadProjectFolderColorIDs()
        if externalColors != projectFolderColorIDs {
            projectFolderColorIDs = externalColors
        }
        let stateFile = loadAppState()
        // Before rebuildTree: node.sessions ordering consults this set.
        let dateSorted = Set(
            (stateFile?.sessionSortModes ?? [:])
                .filter { $0.value == "date" }.keys
        )
        if dateSorted != dateSortedProjectIDs {
            dateSortedProjectIDs = dateSorted
            invalidateSidebarLists()
        }
        let sessions = scanSessions(snapshot: snapshot)
        var projects = (stateFile?.projects ?? []) + ephemeralProjects + nativeProjects(
            excludingPaths: Set((stateFile?.projects ?? []).map(\.path)),
            excludingIDs: Set((stateFile?.projects ?? []).map(\.id))
        )
        // Native "Remove project" tombstones hide Tauri-owned projects (we
        // can't delete them from app-state.json). Worktree children of a
        // removed parent disappear with it — remove_project (project.rs:164)
        // orphans them, and orphaned worktrees never render in the Svelte
        // tree either (topLevelProjects filters worktree_branch).
        let removed = removedProjectIDs(
            prunedAgainst: Set(projects.map(\.id))
        )
        if !removed.isEmpty {
            projects.removeAll {
                removed.contains($0.id)
                    || ($0.parentProjectID.map(removed.contains) ?? false)
            }
        }
        // Claude, Codex, and other tools create ordinary linked Git
        // worktrees. Adopt only those reported by each visible top-level
        // project's own Git registry, so they render through the existing
        // inline child-folder UI. Discovery spawns git off-main and folds
        // any changes in with a follow-up rescan (the call itself never
        // mutates records synchronously anymore).
        _ = syncLinkedWorktreeProjects(from: projects)
        lastScanProjects = projects
        lastScanSessions = sessions
        lastScanTauriPins = stateFile?.pinnedSessions ?? [:]
        hasCompletedScan = true
        _ = rebuildTree(projects: projects, sessions: sessions)

        // Hook/unread bookkeeping for sessions that vanished, then settle
        // any pending unread transitions (sessionUnread.ts reconcile).
        let liveIDs = Set(sessions.map(\.id))
        activity.retainSessions(liveIDs)
        completedSessionIDs.formIntersection(liveIDs)
        pendingUnreadSessions.formIntersection(liveIDs)
        menuAttentionDismissals.formIntersection(liveIDs)
        menuPromptNotificationStates = menuPromptNotificationStates.filter {
            liveIDs.contains($0.key)
        }
        let keptPrewarm = prewarmSessionIDs.filter { id in
            liveIDs.contains(id) && sessionsByID[id]?.isLive == true
        }
        if keptPrewarm != prewarmSessionIDs {
            prewarmSessionIDs = keptPrewarm
        }
        let staleUnread = unreadSessionIDs.subtracting(liveIDs)
        if !staleUnread.isEmpty {
            // Not via removeUnread: that writes a read receipt, and these
            // sessions are gone. Invalidate once for the batch instead.
            unreadSessionIDs.subtract(staleUnread)
            invalidateSidebarLists()
        }
        // Another frontend showed the session to the user: a receipt newer
        // than the last settle clears our dot too.
        for sessionID in unreadSessionIDs.intersection(liveIDs) {
            guard let receipt = Self.readSharedMarker(sessionID, .read)?["read_at"] as? Int64
            else { continue }
            let command = sessionsByID[sessionID]?.command ?? ""
            let settled = Self.latestUnreadActivityAt(
                lifecycleAt: Self.sessionLastRealActivityAtMs(
                    sessionID, command: command
                ),
                alertAt: latestAlertAtMs(for: sessionID)
            ) ?? 0
            if settled <= receipt {
                unreadSessionIDs.remove(sessionID)
                invalidateSidebarLists()
            }
        }
        reconcileUnread()
        // These ids belong to whichever scope the sidebar is showing. A
        // rescan always reads THIS instance's local home, even while another
        // workspace/Host is selected; comparing a Flatsome row against the
        // default home's ids would erase its inline editor/confirm on the
        // next heartbeat. Only local scope owns this scan's disappearance
        // decisions. Remote effects clear their interaction explicitly.
        if selectedHostScope.permitsLocalExecution {
            if let confirming = confirmingRemoveSessionID, !liveIDs.contains(confirming) {
                confirmingRemoveSessionID = nil
            }
            if let confirming = confirmingArchiveSessionID, !liveIDs.contains(confirming) {
                confirmingArchiveSessionID = nil
            }
        }
        // Archived-ids overlay GC: the flag follows the session dir (like the
        // rename overlay), so a removed/expired session never leaves a stale
        // entry behind.
        // Adopt archives written by other frontends (TUI/CLI/phone running
        // app-lessly); our own archive path writes the same marker.
        let markerArchived = liveIDs.filter {
            snapshot.archivedMarkerDirs.contains($0)
        }
        let adopted = markerArchived.subtracting(archivedSessionIDs)
        var archiveOverlayChanged = false
        if !adopted.isEmpty {
            archivedSessionIDs.formUnion(adopted)
            archiveOverlayChanged = true
        }
        // Adopt missing recency stamps for ALL marker-backed archives, not
        // only newly adopted ids. This repairs an old/incomplete overlay and
        // persists a TUI/CLI archive's newest-first position across relaunch.
        for sessionID in markerArchived where archivedAtBySession[sessionID] == nil {
            guard let marker = Self.readSharedMarker(sessionID, .archived),
                  (marker["stamped"] as? Bool ?? true),
                  let at = Self.jsonUInt64(marker["archived_at"])
            else { continue }
            archivedAtBySession[sessionID] = Int64(clamping: at)
            archiveOverlayChanged = true
        }
        let staleArchived = archivedSessionIDs.subtracting(liveIDs)
        if !staleArchived.isEmpty {
            archivedSessionIDs.subtract(staleArchived)
            archiveOverlayChanged = true
        }
        if archiveOverlayChanged {
            persistArchivedSessionIDs()
        }
        // Permanently retire pane links whose Sessions disappeared, exited,
        // or were archived. validatedPaneGroup is a render-time safety net;
        // reconciliation frees survivors for another group and repairs the
        // Controller window's persisted layout.
        reconcilePaneLayout()
        reconcileAppPresentations(stateFile?.appPresentations)
        // A visible "archiving…" row only exists while its stop/reap task is
        // in flight; anything else (task lost to a guard, session dir gone)
        // must not strand a phantom row.
        let keptArchiving = archivingSessionIDs.intersection(
            stoppingArchivedSessionIDs.intersection(archivedSessionIDs)
        )
        // Guarded: an in-place mutation of a @Published set fires
        // objectWillChange even when nothing changes, which would republish
        // the whole store on every rescan.
        if keptArchiving != archivingSessionIDs {
            archivingSessionIDs = keptArchiving
        }
        if !sidebarKeepVisibleSessionIDs.isSubset(of: liveIDs) {
            sidebarKeepVisibleSessionIDs.formIntersection(liveIDs)
        }
        if !sidebarUnpinRecencyBump.keys.allSatisfy(liveIDs.contains) {
            sidebarUnpinRecencyBump = sidebarUnpinRecencyBump.filter {
                liveIDs.contains($0.key)
            }
        }
        if selectedHostScope.permitsLocalExecution,
           let editing = editingSessionID,
           !liveIDs.contains(editing) {
            editingSessionID = nil
        }
        rebuildPins(tauriPins: stateFile?.pinnedSessions ?? [:])
        // Shared knobs (app-state.json) — a TUI edit lands here on the next
        // rescan, which the state-bus ping schedules immediately.
        let stopArchiveMinutes = Self.resolvedAutoStopArchiveMinutes(stateFile)
        if stopArchiveMinutes != autoStopArchiveMinutes {
            autoStopArchiveMinutes = stopArchiveMinutes
        }
        let stoppedLimit = Self.resolvedSidebarStoppedLimit(stateFile)
        if stoppedLimit != sidebarVisibleSessionLimit {
            sidebarVisibleSessionLimit = stoppedLimit
        }
        let titleMode = stateFile?.sessionTitleMode ?? .agent
        if titleMode != sessionTitleMode {
            sessionTitleMode = titleMode
        }
        // First run (no app-state.json yet): seed from the builtin global
        // presets so the app starts with Claude + Codex starred by default
        // (their builtins are quick_launch). A present-but-empty presets list
        // is a deliberate "user cleared everything" state and is left as-is.
        let globalPresets: [Preset]
        if let presets = stateFile?.presets {
            globalPresets = presets
        } else if stateFile == nil {
            globalPresets = Preset.builtinGlobalPresets
        } else {
            globalPresets = []
        }
        // `setupCompleted` (the published var) is only updated further down,
        // so pass the file's value directly — the legacy-preference migration
        // inside rebuildPresets needs it on the very first rescan.
        rebuildPresets(
            globalPresets: globalPresets,
            setupDone: stateFile?.setupCompleted ?? false,
            overlayMigrated: stateFile?.nativePresetOverlayMigrated ?? false,
            allowFold: stateFile != nil
                || !FileManager.default.fileExists(atPath: LaunchConfig.appStateFile.path)
        )
        let editor = nativeCodeEditorOverride() ?? stateFile?.codeEditor ?? "code"
        if editor != codeEditor {
            codeEditor = editor
        }
        let theme = nativeThemeOverride() ?? stateFile?.theme
            ?? Self.inheritedDefaultTheme() ?? .system
        if theme != themePreference {
            themePreference = theme
            applyAppAppearance()
        }
        let tint = Self.savedAppTint()
        if tint != appTint {
            applyAppTint(tint)
        }

        // MCP security is read straight from app-state.json (no overlay): the
        // host reads the same fields, so the toggles have to live in the file.
        let orchestrators = stateFile?.mcpOrchestrators ?? [:]
        if orchestrators != mcpOrchestrators {
            mcpOrchestrators = orchestrators
        }
        let writePolicy = stateFile?.mcpNonChildWriteAccess ?? .ask
        if writePolicy != mcpNonChildWriteAccess {
            mcpNonChildWriteAccess = writePolicy
        }
        let writeApprovals = stateFile?.mcpWriteApprovals ?? [:]
        if writeApprovals != mcpWriteApprovals {
            mcpWriteApprovals = writeApprovals
        }
        let appOpenApprovals = stateFile?.mcpAppOpenApprovals ?? [:]
        if appOpenApprovals != mcpAppOpenApprovals {
            mcpAppOpenApprovals = appOpenApprovals
        }
        let defaultAccess = (stateFile?.mcpDefaultAccess ?? .default).accessLevel
        if defaultAccess != mcpDefaultAccess {
            mcpDefaultAccess = defaultAccess
        }
        let blocked = Set(stateFile?.mcpBlockedProjects ?? [])
        if blocked != mcpBlockedProjectIDs {
            mcpBlockedProjectIDs = blocked
        }
        let browserDefault = stateFile?.browserDefaultAccess ?? .on
        if browserDefault != browserDefaultAccess {
            browserDefaultAccess = browserDefault
        }
        let computerDefault = stateFile?.computerDefaultAccess ?? .ask
        if computerDefault != computerDefaultAccess {
            computerDefaultAccess = computerDefault
        }
        let computerApproved = stateFile?.computerApprovals ?? []
        if computerApproved != computerApprovals {
            computerApprovals = computerApproved
        }
        let browserApproved = stateFile?.browserApprovals ?? []
        if browserApproved != browserApprovals {
            browserApprovals = browserApproved
        }
        let worktreeAccess = stateFile?.mcpWorktreeAccess ?? false
        if worktreeAccess != mcpWorktreeAccess {
            mcpWorktreeAccess = worktreeAccess
        }
        let autoAddBrowserScreenshots = stateFile?.mcpAutoAddBrowserScreenshots ?? true
        if autoAddBrowserScreenshots != mcpAutoAddBrowserScreenshots {
            mcpAutoAddBrowserScreenshots = autoAddBrowserScreenshots
        }
        let engineSettings = stateFile?.browserSettings ?? BrowserSettings()
        if engineSettings != browserSettings {
            browserSettings = engineSettings
        }
        let transcriptOptions = stateFile?.transcriptSettings ?? TranscriptSettings()
        if transcriptOptions != transcriptSettings {
            transcriptSettings = transcriptOptions
        }
        let setupDone = stateFile?.setupCompleted ?? false
        if setupDone != setupCompleted {
            setupCompleted = setupDone
        }
        let linkDisplayName = stateFile?.profileDisplayName ?? ""
        if linkDisplayName != profileDisplayName {
            profileDisplayName = linkDisplayName
        }
        let linkAvatar = stateFile?.profileAvatar ?? ""
        if linkAvatar != profileAvatar {
            profileAvatar = linkAvatar
        }

        if let id = archivedProjectID, projectsByID[id] == nil {
            archivedProjectID = nil
        }

        // Same ownership rule as the session interaction state above: the
        // local project's absence says nothing about the selected Host.
        if selectedHostScope.permitsLocalExecution,
           let confirming = confirmingRemoveProjectID,
           projectsByID[confirming] == nil {
            confirmingRemoveProjectID = nil
        }

        // Keep the file watcher alive for the shared-state overlays.
        rebuildFileWatcher()
        // A small set of native-adapter and shared-record writes still lands
        // through this rescan path during convergence. Wake the worker at the
        // same state-bus edge so its authoritative projection does not wait
        // for the ordinary two-second health poll.
        if localHostClientStarted, selectedHostScope == .local {
            remoteHostRuntime.requestImmediateRefresh()
        }
    }

    /// app-state.json, decode gated on (mtime, size).
    private func loadAppState() -> AppStateFile? {
        guard let stamp = Self.statFile(LaunchConfig.appStateFile.path) else {
            appStateCache = nil
            return nil
        }
        if let cached = appStateCache, cached.stamp == stamp {
            return cached.file
        }
        let file = (try? Data(contentsOf: LaunchConfig.appStateFile))
            .flatMap { try? JSONDecoder().decode(AppStateFile.self, from: $0) }
        appStateCache = (stamp, file)
        return file
    }

    // MARK: - File watching (FSEvents) + busy sweep

    /// Coalesced rescan trigger for file events. Requests keep the earliest
    /// pending deadline, so a slow-lane request never delays a fast one.
    private func scheduleRescan(after delay: TimeInterval = 0.1) {
        guard shouldApplyLocalDiskProjection else { return }
        let deadline = Date().addingTimeInterval(delay)
        if let pending = pendingRescanDeadline, pending <= deadline { return }
        pendingRescanWork?.cancel()
        pendingRescanDeadline = deadline
        let work = DispatchWorkItem { [weak self] in
            guard let self else { return }
            self.pendingRescanDeadline = nil
            self.pendingRescanWork = nil
            self.rescanCollectingOffMain()
        }
        pendingRescanWork = work
        DispatchQueue.main.asyncAfter(deadline: .now() + delay, execute: work)
    }

    /// FSEvents fan-in. `output.bin` appends are by far the noisiest event
    /// source (a couple per second per streaming agent). They can maintain an
    /// already-hook-owned deadline and update recency, but never originate a
    /// spinner. While the 1s busy sweep runs they carry no extra information;
    /// otherwise a ~1s refresh is sufficient. Everything else (manifests,
    /// app-state.json) keeps the fast lane.
    private func handleFileEvents(outputOnly: Bool) {
        if outputOnly {
            scheduleRescan(after: 1.0)
        } else {
            scheduleRescan()
        }
    }

    /// One FSEvents stream (file-level events, 0.5s latency) over:
    /// - ~/.unpeel/app-sessions  (manifests appear/change/vanish; output.bin
    ///   growth maintains hook deadlines and terminal recency),
    /// - ~/.unpeel/app-state.json (projects/pins/global presets),
    /// - ~/.unpeel/session-order.json + project-order.json (a drag in
    ///   another frontend). Without these two, a reorder in the terminal UI
    ///   waited on the 5s safety-net rescan instead of showing up at once.
    /// Rebuilt only when the watched path set changes.
    private func rebuildFileWatcher() {
        // FSEvents cannot watch a path that does not exist, and the order
        // files only appear on someone's first drag — so seed them empty.
        // Cheap, and it means a reorder in another frontend is seen at once
        // rather than whenever the 5s safety net next fires.
        let paneLayoutsURL = LaunchConfig.unpeelDir
            .appendingPathComponent(PaneLayoutController.storageFileName)
        for (url, empty) in [
            (Self.sharedSessionOrderURL, "{}"),
            (Self.sharedProjectOrderURL, "[]"),
            (paneLayoutsURL, "{\"version\":1,\"windows\":{}}"),
        ] where !FileManager.default.fileExists(atPath: url.path) {
            try? Data(empty.utf8).write(to: url, options: .atomic)
        }
        let paths = [
            LaunchConfig.appSessionsDir.path,
            LaunchConfig.appStateFile.path,
            Self.sharedSessionOrderURL.path,
            Self.sharedProjectOrderURL.path,
            paneLayoutsURL.path,
        ]

        guard paths != watchedPaths || fsEventStream == nil else { return }
        watchedPaths = paths
        teardownFileWatcher()

        var context = FSEventStreamContext()
        context.info = Unmanaged.passUnretained(self).toOpaque()
        let callback: FSEventStreamCallback = { _, info, _, eventPaths, _, _ in
            guard let info else { return }
            // Delivered on the main queue (FSEventStreamSetDispatchQueue).
            let store = Unmanaged<UnpeelStore>.fromOpaque(info).takeUnretainedValue()
            // kFSEventStreamCreateFlagUseCFTypes: eventPaths is a CFArray of
            // CFString paths, one per event.
            let paths = unsafeBitCast(eventPaths, to: NSArray.self) as? [String] ?? []
            let outputOnly = !paths.isEmpty
                && paths.allSatisfy {
                    $0.hasSuffix("/output.bin")
                        || $0.hasSuffix("/output-retention.json")
                        || $0.contains("/output-retention.tmp.")
                }
            MainActor.assumeIsolated {
                store.handleFileEvents(outputOnly: outputOnly)
            }
        }
        guard let stream = FSEventStreamCreate(
            nil,
            callback,
            &context,
            paths as CFArray,
            FSEventStreamEventId(kFSEventStreamEventIdSinceNow),
            0.5,
            FSEventStreamCreateFlags(
                kFSEventStreamCreateFlagFileEvents | kFSEventStreamCreateFlagUseCFTypes
            )
        ) else {
            NSLog("[UnpeelNative] FSEvents stream creation failed; timer fallback only")
            return
        }
        FSEventStreamSetDispatchQueue(stream, .main)
        FSEventStreamStart(stream)
        fsEventStream = stream
    }

    private func teardownFileWatcher() {
        guard let stream = fsEventStream else { return }
        FSEventStreamStop(stream)
        FSEventStreamInvalidate(stream)
        FSEventStreamRelease(stream)
        fsEventStream = nil
    }

    private func scanSessions(snapshot: ScanDiskSnapshot) -> [SessionEntry] {
        // The first synchronous scan happened before Local connected and is
        // retained solely as the startup/recovery fallback. Once connected,
        // status comes from Host snapshots; re-running the Swift derivation
        // here would make it a second lifecycle engine even if its rows were
        // normally hidden behind the projected model.
        if localHostClientStarted {
            return lastScanSessions
        }
        // The per-dir filesystem pass (enumeration, manifest stat/decode,
        // output stats, archived markers) happened in the snapshot — on a
        // background queue for scheduled rescans. This derivation phase
        // stays on the main actor: it reads and mutates live store state.
        let root = LaunchConfig.appSessionsDir.path
        for dirName in snapshot.expiredPurgedDirs {
            purgedSessionDirs.removeValue(forKey: dirName)
        }

        let now = Date()
        let loadedTitleOverrides = loadSessionTitleOverrides()
        var titleOverrides = loadedTitleOverrides
        let loadedPendingTitleWrites = loadPendingTitleWrites()
        var pendingTitleWrites = loadedPendingTitleWrites
        var migratedTitleMarker = false
        let loadedDismissedRestartRecommendations = loadRestartRecommendationDismissals()
        let dismissedRestartRecommendations = loadedDismissedRestartRecommendations
        var nextRestartRecommendations: [String: SessionRestartRecommendation] = [:]
        var nextDetectedLocalURLs: [String: [String]] = [:]
        var entries: [SessionEntry] = []
        var seen = Set<String>()

        let overridesUnchangedForReuse =
            scanEntryCacheTitleOverrides == loadedTitleOverrides
        for dirName in snapshot.dirNames {
            let dirPath = root + "/" + dirName
            guard let manifest = snapshot.manifests[dirName] else { continue }

            // Exited-session fast path: identical manifest, title overrides,
            // archived marker, and project-override marker mean the derived
            // entry is identical too — exited entries have no live/activity
            // state feeding them. This keeps the on-main apply phase
            // proportional to LIVE sessions, not total history.
            if snapshot.unchangedManifestDirs.contains(dirName),
               overridesUnchangedForReuse,
               snapshot.archivedMarkerDirs.contains(dirName)
                   == scanEntryCacheArchivedDirs.contains(dirName),
               scanEntryCache[dirName]?.hasResumableState
                   == snapshot.resumableStateDirs.contains(dirName),
               snapshot.projectOverrideStamps[dirName]
                   == scanEntryCacheOverrideStamps[dirName],
               let cachedEntry = scanEntryCache[dirName],
               cachedEntry.status == .exited {
                seen.insert(cachedEntry.id)
                entries.append(cachedEntry)
                continue
            }

            let info = manifest.session
            let live = manifest.state == "running"
                && (manifest.pid.map { kill($0, 0) == 0 } ?? false)
                && Self.manifestPidIdentity(manifest) != .notOurs
            let previousRuntimeGeneration = runtimeLaunchGenerations[info.id]
            let runtimeGenerationAdvanced = previousRuntimeGeneration.map {
                manifest.runtimeLaunchGeneration > $0
            } ?? false
            runtimeLaunchGenerations[info.id] = manifest.runtimeLaunchGeneration
            let runtimeLaunchedAt = manifest.runtimeLaunchedAt.map {
                Date(timeIntervalSince1970: TimeInterval($0) / 1_000)
            }
            if let runtimeLaunchedAt {
                runtimeLaunchCutoffs[info.id] = runtimeLaunchedAt
            } else {
                runtimeLaunchCutoffs.removeValue(forKey: info.id)
            }
            if runtimeGenerationAdvanced {
                // Same Session, new managed agent process: old hook ownership,
                // completion, and menu-dismissal state belong to the prior
                // generation. Preserve a fast replacement hook received after
                // the Host's launch stamp; otherwise the current generation's
                // durable hook seed is re-read below after this reset.
                let preservedHookCompletedTurn = activity.resetForRuntimeLaunch(
                    info.id,
                    runtimeGeneration: manifest.runtimeLaunchGeneration,
                    launchedAt: runtimeLaunchedAt
                )
                if preservedHookCompletedTurn == true {
                    completedSessionIDs.insert(info.id)
                } else {
                    completedSessionIDs.remove(info.id)
                }
                resumingAgentSessionIDs.remove(info.id)
                menuAttentionDismissals.remove(info.id)
                watchForResumeFailure(
                    sessionID: info.id,
                    markers: manifest.resumeFailureMarkers,
                    startOffset: manifest.runtimeLaunchOutputOffset
                )
            }
            let activeRuntimeID = live
                ? manifest.runtime?.currentObservation?.id
                : nil
            let launchTool = SetupTool.detect(in: info.command ?? "")
            let launchRuntime = launchTool?.metadata
            let launchUsesLifecycleHooks = launchTool?.usesLifecycleHooks == true
            // A hook-capable runtime the user started by hand inside a blank/
            // custom terminal is hook-owned too, once its live events latch:
            // provider hook installs are global and the hosted shell exports
            // the session's hook env, so a typed `claude` reports like a
            // launched one. The launch command keeps sole authority over disk
            // seeding and pre-latch suppression below.
            let observedTool = activeRuntimeID.flatMap { SetupTool(rawValue: $0) }
            let hooksOwnActivity = SessionActivityEngine.hasHookActivityAuthority(
                launchCommand: info.command ?? "",
                activeRuntimeID: activeRuntimeID,
                hasActiveApp: live && manifest.activeApp != nil
            )
            if live,
               let recommendation = Self.restartRecommendation(for: manifest),
               dismissedRestartRecommendations[info.id] != recommendation.token {
                nextRestartRecommendations[info.id] = recommendation
            }

            var status: SessionStatus = live ? .idle : .exited
            if live {
                // output.bin is only consulted where its size can maintain
                // state: hook-BUSY sessions (growth re-arms the 5-minute idle
                // deadline), and hook-ATTENTION sessions (growth means the
                // user answered and the agent resumed → clear attention to
                // busy; see SessionActivityEngine.noteOutputAndSweep). Hook-idle
                // sessions ignore output entirely, so they are not stat'ed —
                // except when the runtime descriptor marks Stops as
                // provisional while output keeps growing.
                // In a reusable shell, hook latches are tied to the observed
                // foreground process instead: live hook events carry no
                // runtime identity, so a latch left by a previous run must
                // not claim authority over a newly observed one (a stale
                // busy/attention latch from a killed run would misstate its
                // replacement). A missing dict entry is deliberately not an
                // edge — the first scan after an app restart must keep a
                // latch built from live events already accepted this launch.
                if !launchUsesLifecycleHooks {
                    let observedIdentity = manifest.runtime?.currentObservation
                        .flatMap { observation in
                            observation.id.flatMap {
                                $0.isEmpty
                                    ? nil
                                    : "\($0):\(observation.pid ?? 0):\(observation.pidStartedAt ?? 0)"
                            }
                        } ?? ""
                    if let previous = observedForegroundIdentities[info.id],
                       previous != observedIdentity, !observedIdentity.isEmpty {
                        activity.removeSession(info.id)
                    }
                    observedForegroundIdentities[info.id] = observedIdentity
                }
                // Disk seeding stays launch-command-gated: hook markers do
                // not carry a runtime identity, so a blank shell latches from
                // live events only — an old Claude marker cannot become
                // authority for a later Codex. The hook latch is
                // in-memory-only, so after an app restart a mid-turn managed
                // session would sit spinner-less until its next hook event
                // (often Stop — i.e. never busy). Hook scripts persist their
                // last lifecycle event to disk precisely for this gap;
                // re-seed the latch from it before reading the hook state.
                if launchUsesLifecycleHooks,
                   activity.hookOwnedState(info.id) == nil {
                    // Older Grok hook assets collapsed CLI SessionStart and
                    // prompt submission into "Start". Grok's idle TUI can keep
                    // repainting output.bin, so don't revive those legacy
                    // launch-only seeds from output recency.
                    seedHookActivity(
                        sessionID: info.id,
                        dirPath: dirPath,
                        runtimeGeneration: manifest.runtimeLaunchGeneration,
                        runtimeLaunchedAt: runtimeLaunchedAt,
                        anchorStartEventToOutput:
                            launchRuntime?.anchorStartEventToOutput ?? true
                    )
                }
                let hookStateBefore = hooksOwnActivity
                    ? activity.hookOwnedState(info.id)
                    : nil
                // Output/attention and provisional-Stop semantics live beside
                // each runtime's hook recipe — the stable launch binding
                // first, else the observed foreground runtime. They only
                // maintain state already established by an authoritative hook;
                // output and screen changes never originate Busy.
                let outputPolicyRuntime = launchRuntime ?? observedTool?.metadata
                let allowAttentionClearFromOutput =
                    outputPolicyRuntime?.attentionClearsOnOutput ?? true
                let distrustStops =
                    outputPolicyRuntime?.distrustStopsWhileOutputGrows ?? false
                if hookStateBefore == .busy
                    || hookStateBefore == .attention
                    || (hookStateBefore == .idle && distrustStops) {
                    // The activity signal is consumed as "value changed since
                    // last observation": prefer the host's parsed-screen
                    // change stamp — idle repaint loops that redraw identical
                    // content (grok's idle animation) never advance it — and
                    // fall back to raw output.bin size under older hosts.
                    let signal = manifest.screenChangedAt
                        ?? snapshot.outputSignals[dirName] ?? 0

                    // Signal growth re-arms the hook-busy 5-minute deadline
                    // and expires it when passed (session_activity.rs sweep).
                    activity.noteOutputAndSweep(
                        sessionID: info.id,
                        outputSize: signal,
                        allowAttentionClearFromOutput: allowAttentionClearFromOutput,
                        distrustStops: distrustStops,
                        now: now
                    )
                }

                if hooksOwnActivity,
                   let hookState = activity.hookOwnedState(info.id) {
                    // Hook latch: once a session has produced hook events,
                    // hooks (+ the timeout above) are the only trusted
                    // busy/idle signal — output volume must not flip it
                    // (session_activity.rs:446-457, sessionState.ts
                    // explicitLifecycle).
                    status = hookState
                }

                // Agent-drawn select menus (Claude/Codex numbered prompts) fire
                // no lifecycle hook, so the host detects them from the rendered
                // screen and flags `menu_prompt_active`. Surface it as the
                // attention badge — replacing the busy spinner with the yellow
                // dot — so a waiting menu is glanceable in the sidebar. Skipped
                // when the user turns the detection off in Settings, or
                // dismissed this flag via "Clear attention" (re-armed once the
                // host lowers the flag).
                if !manifest.menuPromptActive {
                    menuAttentionDismissals.remove(info.id)
                }
                let menuNotification = Self.menuPromptNotificationDecision(
                    previous: menuPromptNotificationStates[info.id],
                    runtimeGeneration: manifest.runtimeLaunchGeneration,
                    active: manifest.menuPromptActive,
                    initialAppScan: !hasCompletedScan,
                    detectionEnabled: menuAttentionDetectionEnabled,
                    dismissed: menuAttentionDismissals.contains(info.id),
                    // PermissionRequest is authoritative and already travels
                    // through the same dispatcher. If it won the race, consume
                    // this visual edge without delivering a duplicate.
                    hookAlreadyNeedsInput: activity.hookOwnedState(info.id) == .attention
                )
                menuPromptNotificationStates[info.id] = menuNotification.state
                if menuAttentionDetectionEnabled,
                   manifest.menuPromptActive,
                   !menuAttentionDismissals.contains(info.id),
                   status == .busy || status == .idle {
                    status = .attention
                }

                // Live-probed loopback URLs the host published for this
                // session (titlebar "open local site" chip). Only live
                // sessions surface them — the host cannot retract the list
                // after its process exits.
                if !manifest.detectedLocalURLs.isEmpty {
                    nextDetectedLocalURLs[info.id] = manifest.detectedLocalURLs
                }
            } else {
                activity.removeSession(info.id)
            }

            let command = info.command ?? ""
            let manifestLabel = (info.label?.isEmpty == false)
                ? info.label!
                : (command.isEmpty ? "Terminal" : command)
            // The resolved custom title wins over the manifest label (the
            // backend may keep auto-titling the manifest underneath; a
            // custom title stops that from showing — custom_title parity).
            // A rename from any frontend arrives as a title.json marker. It
            // is the shared truth; UserDefaults is only a pre-marker/write-
            // failure fallback and is retired once the marker is durable.
            let markedTitle = titleMarkerValue(sessionID: info.id, dirPath: dirPath)
            let nativeTitle = Self.normalizedSessionTitle(titleOverrides[info.id])
            let pendingWriteAt = nativeTitle.flatMap { _ in pendingTitleWrites[info.id] }
            let titleResolution = Self.resolvedSessionTitle(
                sharedMarker: markedTitle,
                nativeTitle: nativeTitle,
                pendingWriteAt: pendingWriteAt
            )
            let titleOverride = titleResolution.title
            if titleResolution.shouldPublishNative, let nativeTitle {
                // Retry a failed newer native intent with its original
                // timestamp. Advancing the timestamp on each rescan would let
                // a stale fallback leapfrog a later TUI/CLI marker forever.
                let publishAt = pendingWriteAt.flatMap { $0 > 0 ? $0 : nil }
                    ?? Self.nextTitleIntentTimestamp(after: markedTitle?.updatedAt)
                if publishTitleMarker(
                    sessionID: info.id,
                    title: nativeTitle,
                    updatedAt: publishAt
                ) {
                    titleOverrides.removeValue(forKey: info.id)
                    pendingTitleWrites.removeValue(forKey: info.id)
                    migratedTitleMarker = true
                }
            } else if Self.normalizedSessionTitle(markedTitle?.title) != nil {
                // `title.json` is the cross-frontend truth. Retire the old
                // UserDefaults fallback once any frontend has published a
                // valid marker so it cannot hide a later TUI/CLI rename.
                titleOverrides.removeValue(forKey: info.id)
                pendingTitleWrites.removeValue(forKey: info.id)
            } else if nativeTitle == nil {
                pendingTitleWrites.removeValue(forKey: info.id)
            }
            // Sync only when the decoded manifest disagrees (label or
            // custom_title): the no-op case must stay stat-free — an
            // unconditional call re-reads manifest.json for every titled
            // session on every rescan.
            if let titleOverride,
               info.label != titleOverride || info.customTitle != true {
                syncSessionTitleOverrideToManifest(sessionID: info.id, label: titleOverride)
            }
            let label = titleOverride ?? manifestLabel
            let usesLifecycleHooks = launchUsesLifecycleHooks
            let hookEventAt = usesLifecycleHooks
                ? Self.fileModificationAtMs(dirPath + "/last-hook-event.json")
                : nil
            // Hook-owned agents never consult screen/output here: resize and
            // idle TUI repaint traffic is not a lifecycle event. Hookless
            // tools prefer the host's semantic text-change timestamp, with
            // output mtime retained solely for older manifests.
            let screenChangedAt = usesLifecycleHooks
                ? nil
                : manifest.screenChangedAt.map { Int64(clamping: $0) }
            let outputAt = !usesLifecycleHooks && screenChangedAt == nil
                ? Self.fileModificationAtMs(dirPath + "/output.bin")
                : nil
            let finalExitedAt = manifest.state == "exited" && manifest.updatedAt > 0
                ? Int64(clamping: manifest.updatedAt)
                : nil
            let lifecycleAt = Self.resolvedLifecycleAtMs(
                createdAtMs: info.createdAt ?? 0,
                command: command,
                hookEventAtMs: hookEventAt,
                screenChangedAtMs: screenChangedAt,
                outputAtMs: outputAt,
                finalExitedAtMs: finalExitedAt
            )
            seen.insert(info.id)
            let entry = SessionEntry(
                id: info.id,
                projectID: info.projectID,
                label: label,
                command: command,
                createdAt: info.createdAt ?? 0,
                ownerPrincipalID: info.ownerPrincipalID
                    ?? SessionOwnership.hostOwnerPrincipalID(hostID: localHostID),
                createdByDeviceID: info.createdByDeviceID,
                sourcePresetID: info.sourcePresetID,
                status: status,
                activeRuntimeID: activeRuntimeID,
                activeApp: live ? manifest.activeApp : nil,
                runtimeLaunchPending: manifest.runtimeLaunchPending,
                hostProtocolVersion: manifest.hostProtocolVersion,
                customTitle: titleOverride != nil || (info.customTitle ?? false),
                worktreePath: info.worktreePath,
                worktreeBranch: info.worktreeBranch,
                spawnedBy: info.spawnedBy,
                role: info.role,
                task: info.task,
                providerTranscriptPath: manifest.providerTranscriptPath,
                hasResumableState: snapshot.resumableStateDirs.contains(dirName),
                projectOverrideID: projectOverrideValue(sessionID: info.id, dirPath: dirPath),
                // Quantized to 30s buckets: every consumer is coarse
                // (minute-granular age text, date-level sorting), and the
                // raw stamp advances on every rescan of a streaming session
                // - which made `newNodes != nodes` true 1-2x/s forever, and
                // every publish re-runs the whole sidebar view graph
                // (~50ms of main-queue work). Buckets keep the tree
                // bit-identical between real changes.
                lifecycleAtMs: (lifecycleAt / 30_000) * 30_000,
                cwd: manifest.cwd
            )
            scanEntryCache[dirName] = entry
            entries.append(entry)
        }
        scanEntryCache = scanEntryCache.filter {
            snapshot.manifests[$0.key] != nil
        }
        scanEntryCacheTitleOverrides = loadedTitleOverrides
        scanEntryCacheOverrideStamps = snapshot.projectOverrideStamps
        scanEntryCacheArchivedDirs = snapshot.archivedMarkerDirs

        for id in seen {
            pendingSessions.removeValue(forKey: id)
        }
        for pending in pendingSessions.values where !seen.contains(pending.id) {
            seen.insert(pending.id)
            entries.append(pending)
        }

        Self.publishInstalledAppTints(from: entries)

        observedForegroundIdentities = observedForegroundIdentities.filter { seen.contains($0.key) }
        runtimeLaunchGenerations = runtimeLaunchGenerations.filter { seen.contains($0.key) }
        runtimeLaunchCutoffs = runtimeLaunchCutoffs.filter { seen.contains($0.key) }
        resumingAgentSessionIDs.formIntersection(seen)
        // Session dirs are named by session id; drop cache entries for
        // dirs that no longer exist on disk.
        let dirNames = Set(snapshot.dirNames)
        scanCollector.retainCachedManifests(dirNames)
        titleMarkerCache = titleMarkerCache.filter { dirNames.contains($0.key) }
        projectOverrideCache = projectOverrideCache.filter { dirNames.contains($0.key) }
        // GC rename-overlay entries whose session dir is gone for good
        // (dir existence, not manifest decode success, so a torn manifest
        // write can't drop a rename).
        let keptTitles = titleOverrides.filter { dirNames.contains($0.key) }
        if keptTitles != loadedTitleOverrides {
            saveSessionTitleOverrides(keptTitles)
        }
        let keptPendingTitleWrites = pendingTitleWrites.filter { dirNames.contains($0.key) }
        if keptPendingTitleWrites != loadedPendingTitleWrites {
            savePendingTitleWrites(keptPendingTitleWrites)
        }
        if migratedTitleMarker {
            announceStateChange("session-markers")
        }
        let keptDismissals = dismissedRestartRecommendations.filter { dirNames.contains($0.key) }
        if keptDismissals != loadedDismissedRestartRecommendations {
            saveRestartRecommendationDismissals(keptDismissals)
        }
        // Provider-session-id overlay is written for essentially every hook POST
        // that carries a session_id; GC it here (dir existence) so entries for
        // sessions whose dir vanished — externally, or via another instance's
        // cleanup — don't accumulate in UserDefaults forever. pruneNativeState
        // still handles the explicit remove/restart path.
        let providerIDs = loadProviderSessionIDs()
        let keptProviderIDs = providerIDs.filter { dirNames.contains($0.key) }
        if keptProviderIDs.count != providerIDs.count {
            saveProviderSessionIDs(keptProviderIDs)
        }
        if nextRestartRecommendations != restartRecommendations {
            restartRecommendations = nextRestartRecommendations
        }
        if nextDetectedLocalURLs != detectedLocalURLs {
            detectedLocalURLs = nextDetectedLocalURLs
        }
        if phoneResizeOverrides.keys.contains(where: { !seen.contains($0) }) {
            phoneResizeOverrides = phoneResizeOverrides.filter { seen.contains($0.key) }
        }
        if resumeFailures.contains(where: { !seen.contains($0) }) {
            resumeFailures = resumeFailures.filter { seen.contains($0) }
        }

        // Restart leaves the pre-restart session as a dead row until its host
        // finishes exiting. `killAndCleanup` deletes the old dir and re-sweeps
        // for ~3s to catch the host's final `state=exited` write, but a slow
        // provider (codex/claude) can flush that write after the sweep, so the
        // dir reappears and the dead session lingers as a greyed duplicate.
        // GC it here: restart copies `created_at` exactly onto the replacement,
        // so an exited session sharing (project, created_at) with a live one is
        // that stale leftover. Its host is already gone (it's exited), so
        // removing the dir is safe and can't race a live writer.
        let ghosts = Self.supersededRestartGhostIDs(
            entries.map {
                RestartGhostCandidate(
                    id: $0.id,
                    projectID: $0.projectID,
                    createdAt: $0.createdAt,
                    isLive: $0.isLive
                )
            }
        )
        if !ghosts.isEmpty {
            entries.removeAll { ghosts.contains($0.id) }
            for id in ghosts {
                try? FileManager.default.removeItem(
                    at: LaunchConfig.appSessionsDir.appendingPathComponent(id)
                )
                scanCollector.removeCachedManifest(id)
            }
        }
        return entries
    }

    /// Minimal view of a session for restart-ghost detection (kept tiny so the
    /// pure detection logic is unit-testable without building a `SessionEntry`).
    struct RestartGhostCandidate {
        let id: String
        let projectID: String
        let createdAt: Int64
        let isLive: Bool
    }

    /// Ids of exited sessions that are pre-restart leftovers. Restart copies a
    /// session's `created_at` exactly onto its replacement, so an **exited**
    /// session that shares `(projectID, created_at)` with a **live** session is
    /// the stale old instance a restart left behind. `created_at == 0` is
    /// ignored so timestamp-less manifests never group together, and fork is
    /// unaffected because it deliberately takes a fresh `created_at`.
    nonisolated static func supersededRestartGhostIDs(
        _ candidates: [RestartGhostCandidate]
    ) -> Set<String> {
        var liveKeys = Set<String>()
        for c in candidates where c.isLive && c.createdAt > 0 {
            liveKeys.insert("\(c.projectID)\u{1f}\(c.createdAt)")
        }
        guard !liveKeys.isEmpty else { return [] }
        var ghosts = Set<String>()
        for c in candidates where !c.isLive && c.createdAt > 0 {
            if liveKeys.contains("\(c.projectID)\u{1f}\(c.createdAt)") {
                ghosts.insert(c.id)
            }
        }
        return ghosts
    }

    /// Returns true when the rebuilt sidebar tree differs from the published
    /// one — steady-state rescans of streaming sessions usually produce an
    /// identical tree, and callers use the result to skip work that only
    /// matters when rows actually changed.
    @discardableResult
    private func rebuildTree(projects: [Project], sessions: [SessionEntry]) -> Bool {
        // A session whose removal is in flight vanishes from the sidebar
        // immediately; the kill/cleanup then runs silently in the background
        // (no dimmed "removing" placeholder row).
        var sessions = sessions.filter { !removingSessionIDs.contains($0.id) }
        // A session being restarted keeps its row throughout the teardown +
        // respawn, so it never blinks out of the sidebar (restart gap fix).
        // The snapshot is injected only once the live scan stops producing
        // the row (i.e. after the old host's manifest is deleted and before
        // the replacement's manifest exists).
        if !restartPlaceholders.isEmpty {
            let present = Set(sessions.map(\.id))
            for (id, snapshot) in restartPlaceholders where !present.contains(id) {
                sessions.append(snapshot)
            }
        }

        var byProject: [String: [SessionEntry]] = [:]
        // A project-override marker files the session under another project
        // (group/worktree folder) — display + ordering only, and only when
        // the target still exists; a stale marker falls back to the manifest
        // project instead of orphaning the row.
        let knownProjectIDs = Set(projects.map(\.id))
        for s in sessions {
            let key = s.projectOverrideID.flatMap {
                knownProjectIDs.contains($0) ? $0 : nil
            } ?? s.projectID
            byProject[key, default: []].append(s)
        }
        for key in byProject.keys {
            // Newest-first, exactly sortSessionsNewestFirst in
            // stores/sessions.ts:164-165 (`b.created_at - a.created_at`).
            // New launches therefore land at the TOP of the regular list
            // (the Svelte store also prepends, sessions.ts:546).
            byProject[key]?.sort { $0.createdAt > $1.createdAt }
            // Native drag-reorder overlay: sessions the user ordered by hand
            // keep that order; ids not in the overlay (newer launches) stay
            // newest-first ABOVE the hand-ordered block.
            byProject[key] = applySessionOrderOverlay(byProject[key]!, projectID: key)
        }

        var childrenOf: [String: [Project]] = [:]
        var topLevel: [Project] = []
        for p in projects {
            if let parent = p.parentProjectID {
                childrenOf[parent, default: []].append(p)
            } else {
                topLevel.append(p)
            }
        }
        topLevel.sort { ($0.sortOrder ?? 0) < ($1.sortOrder ?? 0) }
        // Native drag-reorder overlay over the file's sort_order: overlay ids
        // first (in overlay order), unknown/new projects appended in file
        // order — matching reorder_projects/add_project in project.rs, where
        // new projects get max(sort_order)+1 (appended last).
        topLevel = applyProjectOrderOverlay(topLevel, parentID: nil)

        func node(for project: Project) -> ProjectNode {
            // Worktree checkouts AND plain groups (organizational child
            // folders, isFolder + parent, no branch) render as inline
            // folder rows.
            let childProjects = (childrenOf[project.id] ?? [])
                .filter { $0.worktreeBranch != nil || $0.isFolder == true }
                .sorted { ($0.sortOrder ?? 0) < ($1.sortOrder ?? 0) }
            let kids = applyProjectOrderOverlay(childProjects, parentID: project.id)
                .map { node(for: $0) }
            return ProjectNode(
                project: project,
                sessions: byProject[project.id] ?? [],
                worktrees: kids
            )
        }

        let newNodes = topLevel.map { node(for: $0) }
        var index: [String: SessionEntry] = [:]
        for s in sessions { index[s.id] = s }
        var projIndex: [String: Project] = [:]
        for p in projects { projIndex[p.id] = p }

        sessionsByID = index
        if projIndex != projectsByID {
            projectsByID = projIndex
        }

        // History feed: log the live → exited edge. Keyed off statuses this
        // run has already seen, so a startup rescan over long-dead sessions
        // logs nothing; archiving (which stops the host on purpose) is
        // deliberately silent too.
        for (id, session) in index {
            let previous = activityLoggedStatuses[id]
            activityLoggedStatuses[id] = session.status
            if session.status == .exited,
               let previous, previous != .exited,
               !archivedSessionIDs.contains(id),
               !restartingSessionIDs.contains(id) {
                logActivity(.exited, sessionID: id)
            }
        }
        activityLoggedStatuses = activityLoggedStatuses.filter { index[$0.key] != nil }

        // Repair pane membership before dropping a vanished selection. If a
        // dead representative had two or more surviving members, this keeps
        // the window on that group by promoting its first remaining pane.
        if selectedHostScope == .local,
           let selectedSessionID,
           index[selectedSessionID] == nil {
            reconcilePaneLayout()
        }
        // RootView's surface pruning now sees either the promoted Session or
        // no selection. Remote scope owns its own ids; a local scan must never
        // clear those.
        if selectedHostScope == .local, let sel = selectedSessionID, index[sel] == nil {
            selectedSessionID = nil
        }

        // Only publish when something observable changed (cheap diff).
        let nodesChanged = newNodes != nodes
        if nodesChanged {
            nodes = newNodes
            invalidateSidebarLists()
        }

        refreshTitlebarBranch()
        return nodesChanged
    }

    /// Persist the Local content-pane Session so a relaunch can restore it.
    /// Nil and project-sidebar members are ignored so a panel click or a
    /// Host-scope switch cannot wipe the last real main-area Session.
    private func persistLocalSelectedSessionIfNeeded() {
        guard selectedHostScope == .local, let id = selectedSessionID else { return }
        guard sessionsByID[id] != nil || pendingSessions[id] != nil else { return }
        if sessionIsInProjectSidebar(id) { return }
        AppDefaults.shared.set(id, forKey: Self.selectedSessionKey)
    }

    /// After the first disk scan: reopen the last Local content-pane Session
    /// if it is still present and visible. Missing/archived ids stay empty.
    private func restorePersistedSessionSelection() {
        guard selectedHostScope == .local, selectedSessionID == nil else { return }
        guard let id = AppDefaults.shared.string(forKey: Self.selectedSessionKey),
              !id.isEmpty,
              let session = sessionsByID[id],
              !isHiddenArchived(id),
              !sessionIsInProjectSidebar(id)
        else { return }
        prepareSidebarToRenderSession(session)
        selectedSessionID = id
        let scrollID = validatedPaneGroup(containingSession: id)?
            .representativeSessionID ?? id
        requestSidebarScroll(to: scrollID, centered: false)
    }

    /// Publish in-memory starting sessions immediately, without waiting for
    /// the filesystem watcher or manifest poll. The next full rescan will
    /// merge the same pending rows with app-state.json and manifest state.
    private func publishPendingSessions() {
        let currentSessions = sessionsByID.values.filter { pendingSessions[$0.id] == nil }
        rebuildTree(
            projects: currentProjectsInDisplayOrder(),
            sessions: Array(currentSessions) + Array(pendingSessions.values)
        )
    }

    private func currentProjectsInDisplayOrder() -> [Project] {
        var projects: [Project] = []
        func walk(_ nodes: [ProjectNode]) {
            for node in nodes {
                projects.append(node.project)
                walk(node.worktrees)
            }
        }
        walk(nodes)
        return projects.isEmpty ? Array(projectsByID.values) : projects
    }

    // MARK: - Expansion

    func toggleProjectExpanded(_ projectID: String) {
        if expandedProjectIDs.contains(projectID) {
            expandedProjectIDs.remove(projectID)
            // Collapsing drops any "keep this hidden row visible" pins for
            // the project — reopening starts from the plain recent window.
            let projectSessionIDs = Set(
                (findDisplayNode(projectID)?.sessions ?? []).map(\.id)
            )
            if !sidebarKeepVisibleSessionIDs.isDisjoint(with: projectSessionIDs) {
                sidebarKeepVisibleSessionIDs.subtract(projectSessionIDs)
            }
        } else {
            expandedProjectIDs.insert(projectID)
        }
    }

    func revealSessionInSidebar(_ sessionID: String) {
        guard selectedHostScope == .local else { return }
        let navigationID = validatedPaneGroup(containingSession: sessionID)?
            .representativeSessionID ?? sessionID
        guard let session = sessionsByID[navigationID] else { return }

        closeSettings()
        prepareSidebarToRenderSession(session)

        if selectedSessionID != sessionID {
            selectedSessionID = sessionID
        }

        requestSidebarScroll(to: navigationID, centered: true)
    }

    /// Follow a row that just moved in place (pinning teleports it up into
    /// the project's pinned section): minimal scroll so the row stays in
    /// view, without changing selection or expansion state.
    func followSessionRowInSidebar(_ sessionID: String) {
        requestSidebarScroll(to: sessionID, centered: false)
    }

    private func requestSidebarScroll(to sessionID: String, centered: Bool) {
        sidebarSessionRevealSerial += 1
        let request = SidebarSessionRevealRequest(
            sessionID: sessionID,
            serial: sidebarSessionRevealSerial,
            centered: centered
        )
        DispatchQueue.main.async { [weak self] in
            guard let self, self.sessionsByID[sessionID] != nil else { return }
            self.sidebarSessionRevealRequest = request
        }
        DispatchQueue.main.asyncAfter(deadline: .now() + 1) { [weak self] in
            guard let self, self.sidebarSessionRevealRequest == request else { return }
            self.sidebarSessionRevealRequest = nil
        }
    }

    private func prepareSidebarToRenderSession(_ session: SessionEntry) {
        // A project-override files the row under a group/worktree node, so
        // expansion and the window check must target that node, not the
        // manifest's launch project.
        let projectID = effectiveProjectID(for: session)
        guard let project = projectsByID[projectID] else { return }

        // Worktree children render inline under their parent: expand every
        // ancestor so the session's project row is actually visible.
        var ancestorID = project.parentProjectID
        while let id = ancestorID {
            expandedProjectIDs.insert(id)
            ancestorID = projectsByID[id]?.parentProjectID
        }
        expandedProjectIDs.insert(projectID)

        guard let node = findNode(projectID) else { return }
        let pinnedIDs = Set(pinnedSessions(in: node).map(\.id))
        guard !pinnedIDs.contains(session.id) else { return }
        let displayedIDs = Set(displayedSessions(in: node).map(\.id))
        if !displayedIDs.contains(session.id) {
            // Beyond the inactive-preview window: pin just this row visible.
            sidebarKeepVisibleSessionIDs.insert(session.id)
        }
    }

    // MARK: - Project folder colors

    func projectFolderColor(for projectID: String) -> ProjectFolderColor? {
        let raw = remoteProjectSummariesByID[projectID]?.colorID
            ?? projectFolderColorIDs[projectID]
        guard let raw else { return nil }
        return ProjectFolderColor(rawValue: raw)
    }

    func setProjectFolderColor(_ color: ProjectFolderColor?, for projectID: String) {
        if routesProjectVerbThroughHost(projectID) {
            performRemoteVerb("Couldn't change the folder color") { runtime in
                try await runtime.setProjectFolderColor(
                    projectID: projectID,
                    colorID: color?.rawValue
                )
            }
            return
        }
        if let color {
            projectFolderColorIDs[projectID] = color.rawValue
        } else {
            projectFolderColorIDs.removeValue(forKey: projectID)
        }
        saveProjectFolderColorIDs()
    }

    /// Whether this group's sessions sort by date (recently updated first)
    /// instead of the manual drag order.
    func isDateSorted(projectID: String) -> Bool {
        if let summary = remoteProjectSummariesByID[projectID] {
            return summary.dateSorted == true
        }
        return dateSortedProjectIDs.contains(projectID)
    }

    /// Flip a group between date sort and custom order. The mode lives in
    /// app-state.json (`session_sort_modes`) so the TUI reads and writes the
    /// same truth; the manual order in session-order.json stays untouched,
    /// so switching back to custom restores the old arrangement.
    func setSessionDateSorted(_ dateSorted: Bool, for projectID: String) {
        if routesProjectVerbThroughHost(projectID) {
            performRemoteVerb("Couldn't change the Session sort") { runtime in
                try await runtime.setProjectDateSorted(
                    projectID: projectID,
                    dateSorted: dateSorted
                )
            }
            return
        }
        let wrote = editPresetStateAnnouncing { object in
            var modes = (object["session_sort_modes"] as? [String: Any])?
                .compactMapValues { $0 as? String } ?? [:]
            if dateSorted {
                modes[projectID] = "date"
            } else {
                modes.removeValue(forKey: projectID)
            }
            object["session_sort_modes"] = modes
        }
        guard wrote else { return }
        if dateSorted {
            dateSortedProjectIDs.insert(projectID)
        } else {
            dateSortedProjectIDs.remove(projectID)
        }
        invalidateSidebarLists()
        withAnimation(.easeInOut(duration: 0.18)) { rebuildTreeFromLastScan() }
    }

    private static func loadProjectFolderColorIDs() -> [String: String] {
        let raw = AppDefaults.shared.dictionary(forKey: nativeProjectFolderColorsKey) ?? [:]
        return raw.compactMapValues { value in
            guard let string = value as? String,
                  ProjectFolderColor(rawValue: string) != nil
            else { return nil }
            return string
        }
    }

    private func saveProjectFolderColorIDs() {
        if projectFolderColorIDs.isEmpty {
            AppDefaults.shared.removeObject(forKey: Self.nativeProjectFolderColorsKey)
        } else {
            AppDefaults.shared.set(
                projectFolderColorIDs, forKey: Self.nativeProjectFolderColorsKey
            )
        }
    }

    // MARK: - Pins

    /// Native-side pin intent persisted until the matching shared-state write
    /// succeeds. Older builds kept these entries forever; `removedAt` and the
    /// reconciliation below let newer shared changes supersede those stale
    /// overlays while preserving an intent whose disk write actually failed.
    struct NativePinOverrides: Codable, Equatable {
        var added: [PinnedSidebarSession] = []
        var removedKeys: [String] = []
        var removedAt: [String: UInt64] = [:]

        init(
            added: [PinnedSidebarSession] = [],
            removedKeys: [String] = [],
            removedAt: [String: UInt64] = [:]
        ) {
            self.added = added
            self.removedKeys = removedKeys
            self.removedAt = removedAt
        }

        private enum CodingKeys: String, CodingKey {
            case added, removedKeys, removedAt
        }

        init(from decoder: Decoder) throws {
            let container = try decoder.container(keyedBy: CodingKeys.self)
            added = try container.decodeIfPresent(
                [PinnedSidebarSession].self, forKey: .added
            ) ?? []
            removedKeys = try container.decodeIfPresent(
                [String].self, forKey: .removedKeys
            ) ?? []
            // Absent in every previously shipped overlay. A legacy tombstone
            // is retired as soon as readable shared state confirms either the
            // pin or its absence; see `reconciledPinOverrides`.
            removedAt = try container.decodeIfPresent(
                [String: UInt64].self, forKey: .removedAt
            ) ?? [:]
        }
    }

    private func loadPinOverrides() -> NativePinOverrides {
        guard let data = AppDefaults.shared.data(forKey: Self.nativePinsKey),
              let overrides = try? JSONDecoder().decode(NativePinOverrides.self, from: data)
        else { return NativePinOverrides() }
        return overrides
    }

    private func savePinOverrides(_ overrides: NativePinOverrides) {
        if overrides.added.isEmpty && overrides.removedKeys.isEmpty {
            AppDefaults.shared.removeObject(forKey: Self.nativePinsKey)
            return
        }
        if let data = try? JSONEncoder().encode(overrides) {
            AppDefaults.shared.set(data, forKey: Self.nativePinsKey)
        }
    }

    static func reconciledPinOverrides(
        _ overrides: NativePinOverrides,
        sharedPins: [String: PinnedSidebarSession],
        sharedStateModifiedAt: UInt64?
    ) -> NativePinOverrides {
        var reconciled = overrides

        // An added overlay is pending only while it is newer than what the
        // shared file proves. A same/newer shared pin confirms the write; a
        // newer shared file that omits it represents an external unpin.
        reconciled.added.removeAll { nativePin in
            if let sharedPin = sharedPins[nativePin.key] {
                return max(sharedPin.pinnedAt, sharedStateModifiedAt ?? 0)
                    >= nativePin.pinnedAt
            }
            guard let sharedStateModifiedAt else { return false }
            return sharedStateModifiedAt >= nativePin.pinnedAt
        }

        var seen = Set<String>()
        reconciled.removedKeys = reconciled.removedKeys.filter { key in
            guard seen.insert(key).inserted else { return false }
            guard let removedAt = reconciled.removedAt[key] else {
                // Legacy removals had no timestamp. Once shared state is
                // readable it is the only safe authority: presence means a
                // later repin, absence means the old unpin already landed.
                return sharedStateModifiedAt == nil
            }
            if let sharedPin = sharedPins[key] {
                return max(sharedPin.pinnedAt, sharedStateModifiedAt ?? 0)
                    <= removedAt
            }
            guard let sharedStateModifiedAt else { return true }
            return sharedStateModifiedAt < removedAt
        }
        let retainedRemovalKeys = Set(reconciled.removedKeys)
        reconciled.removedAt = reconciled.removedAt.filter {
            retainedRemovalKeys.contains($0.key)
        }
        return reconciled
    }

    private static func appStateModifiedAtUnixMs() -> UInt64? {
        guard let stamp = statFile(LaunchConfig.appStateFile.path),
              stamp.mtimeSec >= 0,
              stamp.mtimeNsec >= 0
        else { return nil }
        return UInt64(stamp.mtimeSec) * 1_000
            + UInt64(stamp.mtimeNsec) / 1_000_000
    }

    private static func nextPinIntentTimestamp() -> UInt64 {
        let now = UInt64(Date().timeIntervalSince1970 * 1_000)
        guard let shared = appStateModifiedAtUnixMs(), shared >= now else { return now }
        return shared == UInt64.max ? shared : shared + 1
    }

    func isPinned(sessionID: String, projectID: String) -> Bool {
        if let summary = remoteSummariesByID[sessionID] {
            return summary.projectID == projectID && summary.pinned
        }
        return pinnedByProject[projectID]?.contains {
            $0.key == PinnedSidebarSession.key(forSessionID: sessionID)
        } ?? false
    }

    /// Whether a Session can expose Pin/Unpin in its context menu. Remote
    /// scopes require the Host-advertised operation; local pins use the
    /// shared app-state contract directly.
    func sessionCanPin(_ sessionID: String) -> Bool {
        guard routesSessionVerbThroughHost(sessionID) else { return true }
        return remoteHostRuntime.supportsHostOperation(
            RemoteHostRuntime.HostOperation.pinSet
        )
    }

    /// Plain groups carry their own additive `pinned_at` marker. A remote
    /// summary is Host truth; local state comes from the decoded project row.
    func isGroupPinned(_ projectID: String) -> Bool {
        guard (displayProjectsByID[projectID] ?? projectsByID[projectID])?
            .acceptsSessionDrop == true
        else { return false }
        if displaysHostProjection,
           let summary = remoteProjectSummariesByID[projectID] {
            return summary.pinned == true
        }
        return projectsByID[projectID]?.pinnedAt != nil
    }

    func groupCanPin(_ projectID: String) -> Bool {
        guard (displayProjectsByID[projectID] ?? projectsByID[projectID])?
            .acceptsSessionDrop == true
        else { return false }
        guard routesProjectVerbThroughHost(projectID) else { return true }
        return remoteHostRuntime.supportsHostOperation(
            RemoteHostRuntime.HostOperation.projectPinSet
        )
    }

    func setGroupPinned(_ projectID: String, pinned: Bool) {
        if routesProjectVerbThroughHost(projectID) {
            performRemoteVerb(pinned ? "Couldn't pin the group" : "Couldn't unpin the group") {
                runtime in
                try await runtime.setProjectPinned(projectID: projectID, pinned: pinned)
            }
            return
        }
        setLocalGroupPinned(projectID, pinned: pinned)
    }

    /// Host-local half used by both the desktop context menu and inbound
    /// Controller effects. Never consults the currently selected remote scope.
    @discardableResult
    private func setLocalGroupPinned(_ projectID: String, pinned: Bool) -> Bool {
        guard projectsByID[projectID]?.acceptsSessionDrop == true else { return false }
        // Native-created groups are mirrored into app-state. Re-run the
        // idempotent mirror before editing so the pin marker always has one
        // shared record to live on.
        mirrorProjectsToSharedState()
        var applied = false
        let wrote = editPresetStateAnnouncing { object in
            var projects = (object["projects"] as? [[String: Any]]) ?? []
            guard let index = projects.firstIndex(where: {
                ($0["id"] as? String) == projectID
                    && ($0["is_folder"] as? Bool) == true
                    && (($0["parent_project_id"] as? String)?
                        .trimmingCharacters(in: .whitespacesAndNewlines).isEmpty == false)
                    && ($0["worktree_branch"] == nil
                        || $0["worktree_branch"] is NSNull)
            }) else { return }
            let stamp = Self.nextPinIntentTimestamp()
            if pinned {
                if projects[index]["pinned_at"] == nil {
                    projects[index]["pinned_at"] = stamp
                }
            } else {
                projects[index].removeValue(forKey: "pinned_at")
            }
            object["projects"] = projects
            // Keep the unified pinned ORDER record in `pinned_sessions`
            // aligned with the marker: pinning appends a "project:" row at
            // the bottom of the parent's mixed pinned partition, unpinning
            // retires its rank. The `pinned_at` project marker above stays
            // the cross-frontend pin truth; the record is ordering only.
            let recordKey = PinnedSidebarSession.key(forProjectID: projectID)
            let parentID = (projects[index]["parent_project_id"] as? String) ?? ""
            let recordEdit = pinned
                ? NativePinOverrides(added: [PinnedSidebarSession(
                    key: recordKey,
                    projectID: parentID,
                    sessionID: nil,
                    pinnedAt: stamp
                )])
                : NativePinOverrides(
                    removedKeys: [recordKey], removedAt: [recordKey: stamp]
                )
            Self.applyPinOverrides(recordEdit, to: &object)
            applied = true
        }
        guard wrote, applied else { return false }
        rescan()
        return true
    }

    /// Project whose sidebar node currently owns the session. A valid shared
    /// override wins; a removed/stale group falls back to the manifest's
    /// launch project, exactly like `rebuildTree`.
    private func effectiveProjectID(for session: SessionEntry) -> String {
        session.projectOverrideID.flatMap {
            projectsByID[$0] != nil ? $0 : nil
        } ?? session.projectID
    }

    func pinSession(projectID: String, sessionID: String) {
        if routesSessionVerbThroughHost(sessionID) {
            performRemoteVerb("Couldn't pin the session") { runtime in
                try await runtime.setSessionPinned(sessionID, pinned: true)
            }
            return
        }
        let key = PinnedSidebarSession.key(forSessionID: sessionID)
        var overrides = loadPinOverrides()
        overrides.removedKeys.removeAll { $0 == key }
        overrides.removedAt.removeValue(forKey: key)
        overrides.added.removeAll { $0.key == key }
        overrides.added.append(PinnedSidebarSession(
            key: key,
            projectID: projectID,
            sessionID: sessionID,
            pinnedAt: Self.nextPinIntentTimestamp()
        ))
        savePinOverrides(overrides)
        rebuildPins(tauriPins: loadAppState()?.pinnedSessions ?? [:])
        announceStateChange("app-state")
    }

    func unpinSession(projectID _: String, sessionID: String) {
        if routesSessionVerbThroughHost(sessionID) {
            performRemoteVerb("Couldn't unpin the session") { runtime in
                try await runtime.setSessionPinned(sessionID, pinned: false)
            }
            return
        }
        let key = PinnedSidebarSession.key(forSessionID: sessionID)
        var overrides = loadPinOverrides()
        overrides.added.removeAll { $0.key == key }
        if !overrides.removedKeys.contains(key) {
            overrides.removedKeys.append(key)
        }
        overrides.removedAt[key] = Self.nextPinIntentTimestamp()
        // Resurface the row instead of dropping it: an old archived Session
        // lives past the preview window, so a plain unpin used to hide it
        // outright. Stamp a fresh recency (top of its stopped/archive block,
        // right below the live rows) and force it visible past the window.
        sidebarUnpinRecencyBump[sessionID] = Int64(Date().timeIntervalSince1970 * 1_000)
        sidebarKeepVisibleSessionIDs.insert(sessionID)
        savePinOverrides(overrides)
        rebuildPins(tauriPins: loadAppState()?.pinnedSessions ?? [:])
        announceStateChange("app-state")
    }

    /// Merge shared pins with native write-failure fallbacks, then retire the
    /// fallbacks once the merged state is durably mirrored. Shared changes
    /// newer than an overlay win, so App -> TUI -> App handoff cannot revive
    /// an old pin or unpin. Rendering remains oldest-first so a newly pinned
    /// session lands at the bottom of the pin list.
    private func rebuildPins(tauriPins: [String: [PinnedSidebarSession]]) {
        let loadedOverrides = loadPinOverrides()
        var sharedByKey: [String: PinnedSidebarSession] = [:]
        for pin in tauriPins.values.joined() {
            if let current = sharedByKey[pin.key], current.pinnedAt > pin.pinnedAt {
                continue
            }
            sharedByKey[pin.key] = pin
        }
        var overrides = Self.reconciledPinOverrides(
            loadedOverrides,
            sharedPins: sharedByKey,
            sharedStateModifiedAt: Self.appStateModifiedAtUnixMs()
        )
        let removed = Set(overrides.removedKeys)

        var merged: [String: PinnedSidebarSession] = [:]
        for pin in sharedByKey.values where !removed.contains(pin.key) {
            merged[pin.key] = pin
        }
        for pin in overrides.added {
            merged[pin.key] = pin
        }
        if mirrorPinsToSharedState(overrides) {
            // The pending intents are now reflected in the latest locked
            // shared object. Clear the write-ahead fallback only after that
            // succeeds; a corrupt/unwritable file leaves it recoverable.
            overrides = NativePinOverrides()
        }

        // Garbage-collect native overrides for sessions whose artifact dirs are
        // gone for good. Do not key this off `sessionsByID`: a torn manifest
        // write or transient scan miss must not permanently unpin a session.
        let beforeAdded = overrides.added.count
        overrides.added.removeAll { pin in
            guard let sessionID = pin.sessionID else { return true }
            return sessionsByID[sessionID] == nil
                && pendingSessions[sessionID] == nil
                && !sessionArtifactsExist(sessionID)
        }
        if overrides.added.count != beforeAdded {
            overrides.removedAt = overrides.removedAt.filter { key, _ in
                overrides.removedKeys.contains(key)
            }
        }
        if overrides != loadedOverrides {
            savePinOverrides(overrides)
        }

        // A record is kept only while its target still belongs to that
        // project: a session pin follows the session's effective group; a
        // "project:" group record needs the group to still exist under this
        // parent WITH its cross-frontend `pinned_at` marker (a TUI unpin
        // drops the marker, which retires the record's rank too).
        func recordIsValid(_ pin: PinnedSidebarSession) -> Bool {
            if let sessionID = pin.sessionID {
                guard let session = sessionsByID[sessionID] else { return false }
                return effectiveProjectID(for: session) == pin.projectID
            }
            guard let groupID = pin.pinnedProjectID,
                  let group = projectsByID[groupID]
            else { return false }
            return group.acceptsSessionDrop
                && group.parentProjectID == pin.projectID
                && group.pinnedAt != nil
        }

        // The per-project records ARRAY is the durable mixed pinned order
        // (sessions and pinned child groups interleaved), so the file's
        // array order is preserved as the base instead of re-sorting by
        // pinned_at — for never-reordered files the two are identical (pins
        // append with a monotonic timestamp). Records living only in the
        // native overlay append below, oldest first, so a freshly pinned
        // row lands at the bottom of the pin list.
        var grouped: [String: [PinnedSidebarSession]] = [:]
        var placed = Set<String>()
        for (fileProjectID, filePins) in tauriPins {
            for filePin in filePins {
                guard let pin = merged[filePin.key],
                      pin.projectID == fileProjectID,
                      placed.insert(pin.key).inserted,
                      recordIsValid(pin)
                else { continue }
                grouped[pin.projectID, default: []].append(pin)
            }
        }
        let unplaced = merged.values
            .filter { !placed.contains($0.key) && recordIsValid($0) }
            .sorted {
                $0.pinnedAt != $1.pinnedAt ? $0.pinnedAt < $1.pinnedAt : $0.key < $1.key
            }
        for pin in unplaced {
            grouped[pin.projectID, default: []].append(pin)
        }
        for key in grouped.keys {
            // The combined cross-frontend order wins when it contains pin
            // ranks; the legacy native-only pin overlay remains a migration
            // fallback. Newly-pinned sessions append below the ordered block.
            grouped[key] = applyPinnedOrderOverlay(grouped[key] ?? [], projectID: key)
        }

        if grouped != pinnedByProject {
            pinnedByProject = grouped
            invalidateSidebarLists()
        }
    }

    private func sessionArtifactsExist(_ sessionID: String) -> Bool {
        FileManager.default.fileExists(
            atPath: LaunchConfig.appSessionsDir
                .appendingPathComponent(sessionID, isDirectory: true)
                .path
        )
    }

    /// Reorder pins by the `pinnedOrder` overlay (mirrors
    /// `applySessionOrderOverlay`): overlay entries form a hand-ordered block,
    /// pins not in the overlay keep oldest-first BELOW it so a freshly pinned
    /// session lands at the bottom.
    private func applyPinnedOrderOverlay(
        _ base: [PinnedSidebarSession], projectID: String
    ) -> [PinnedSidebarSession] {
        Self.orderedPinnedSessions(
            base,
            sharedOrder: sessionOrderPreviews[projectID]
                ?? Self.sharedSessionOrder(projectID: projectID),
            localOrder: AppDefaults.shared.stringArray(forKey: Self.pinnedOrderKey(projectID))
        )
    }

    static func orderedPinnedSessions(
        _ base: [PinnedSidebarSession],
        sharedOrder: [String]?,
        localOrder: [String]?
    ) -> [PinnedSidebarSession] {
        // Records rank by their sidebar row id: the session id, or the
        // pinned child group's project id for "project:" ordering records.
        let baseIDs = Set(base.compactMap(\.orderTargetID))
        let sharedKnown = sharedOrder?.filter { baseIDs.contains($0) } ?? []
        let overlay = !sharedKnown.isEmpty ? sharedKnown : (localOrder ?? [])
        guard !overlay.isEmpty else { return base }
        let known = overlay.filter { baseIDs.contains($0) }
        guard !known.isEmpty else { return base }
        var rank: [String: Int] = [:]
        for (index, id) in known.enumerated() { rank[id] = index }
        let rest = base.filter { $0.orderTargetID.map { rank[$0] == nil } ?? true }
        let ordered = base.filter { $0.orderTargetID.map { rank[$0] != nil } ?? false }
            .sorted { rank[$0.orderTargetID ?? ""]! < rank[$1.orderTargetID ?? ""]! }
        return ordered + rest
    }

    // MARK: - Drag-reorder overlays (native; app-state.json is read-only)

    /// The Svelte app persists project order as `sort_order` via the Tauri
    /// `reorder_projects` command (project.rs:227-260) and has NO
    /// within-project session reordering (dragging a session there moves it
    /// to another project). Natively both orders are UserDefaults overlays
    /// merged over the file/derived order at read time:
    /// - `unpeel.native.projectOrder`             = [top-level project ids]
    /// - `unpeel.native.projectOrder.<projectID>` = [worktree child project ids]
    /// - `unpeel.native.sessionOrder.<projectID>` = [session ids]
    /// Ids absent from an overlay keep file/derived order (projects append
    /// last like add_project's max+1 sort_order; sessions stay newest-first
    /// on top). Stale ids are GC'd when a new order is persisted and lazily
    /// pruned at read time.
    static let projectOrderKey = "unpeel.native.projectOrder"

    static func projectOrderKey(forParent parentID: String?) -> String {
        guard let parentID else { return projectOrderKey }
        return "\(projectOrderKey).\(parentID)"
    }

    static func sessionOrderKey(_ projectID: String) -> String {
        "unpeel.native.sessionOrder.\(projectID)"
    }

    /// One combined shared rank list. Each frontend filters it into pinned,
    /// running, and stopped buckets, so publishing one bucket must preserve
    /// the ranks belonging to all the others.
    /// Place `ids` immediately after `hostID`. Existing occurrences of
    /// those ids are removed first. If the host is missing, the ids append.
    static func inserting(_ ids: [String], below hostID: String, in list: [String]) -> [String] {
        let moving = ids.filter { $0 != hostID }
        guard !moving.isEmpty else { return list }
        var result = list.filter { !moving.contains($0) }
        if let index = result.firstIndex(of: hostID) {
            result.insert(contentsOf: moving, at: index + 1)
        } else {
            result.append(contentsOf: moving)
        }
        return result
    }

    static func combinedSessionOrder(pinnedIDs: [String], regularIDs: [String]) -> [String] {
        var seen = Set<String>()
        return (pinnedIDs + regularIDs).filter { seen.insert($0).inserted }
    }

    /// Replace only the slots occupied by `preferred` ids. This lets a held
    /// Session order move around child-folder ids in a mixed sidebar rank
    /// without moving those folders themselves.
    nonisolated static func applyingRelativeIDOrder(
        _ preferred: [String], to base: [String]
    ) -> [String] {
        let known = Set(base)
        var iterator = preferred.filter { known.contains($0) }.makeIterator()
        let replacing = Set(preferred)
        return base.map { id in
            replacing.contains(id) ? (iterator.next() ?? id) : id
        }
    }

    static func replacingSessionID(
        in order: [String]?, oldID: String, newID: String
    ) -> [String]? {
        guard var order, let rank = order.firstIndex(of: oldID) else { return nil }
        order[rank] = newID
        return order
    }

    /// Legacy native pin-order fallback. New writes also publish the combined
    /// shared session order so the TUI sees them; this key remains separate
    /// from the regular local overlay for migration and bucket isolation.
    static func pinnedOrderKey(_ projectID: String) -> String {
        "unpeel.native.pinnedOrder.\(projectID)"
    }

    /// `~/.unpeel/project-order.json` — a flat rank list for every project.
    /// Filtering it by parent yields top-level or child-folder sibling order,
    /// shared with the terminal UI exactly as `session-order.json` is. Cached
    /// against the file's modification date: this is read on every sidebar
    /// rebuild, so the unchanged case must cost a stat.
    private nonisolated(unsafe) static var sharedProjectOrderCache: (stamp: Date, ids: [String])?

    static var sharedProjectOrderURL: URL {
        LaunchConfig.unpeelDir.appendingPathComponent("project-order.json")
    }

    static func sharedProjectOrder() -> [String]? {
        let url = sharedProjectOrderURL
        let stamp = (try? FileManager.default.attributesOfItem(atPath: url.path))?[.modificationDate] as? Date
        guard let stamp else {
            sharedProjectOrderCache = nil
            return nil
        }
        if sharedProjectOrderCache?.stamp != stamp {
            let ids = (try? Data(contentsOf: url))
                .flatMap { try? JSONSerialization.jsonObject(with: $0) } as? [String] ?? []
            sharedProjectOrderCache = (stamp, ids)
        }
        let ids = sharedProjectOrderCache?.ids ?? []
        return ids.isEmpty ? nil : ids
    }

    /// Tell every other Unpeel that shared state moved — the Swift half of
    /// `unpeel-core::state_bus`. Same registry (`~/.unpeel/app-ports`), same
    /// route, our own port skipped. Fire-and-forget: a peer that has gone is
    /// normal, and nothing here may delay a UI action.
    nonisolated static func announceStateChange(_ change: String, ownPort: UInt16?) {
        announceStateChange(
            change,
            registry: LaunchConfig.unpeelDir.appendingPathComponent("app-ports"),
            ownPort: ownPort
        )
    }

    /// Announce to an explicit `app-ports` registry — used so a
    /// local-against-home write against a SCOPED workspace pings THAT
    /// workspace's peers (its own app instance / TUI), not this instance's.
    nonisolated static func announceStateChange(
        _ change: String, registry: URL?, ownPort: UInt16?
    ) {
        guard let registry else { return }
        guard let raw = try? String(contentsOf: registry, encoding: .utf8) else { return }
        let ports = raw.split(whereSeparator: \.isNewline)
            .compactMap { UInt16($0.trimmingCharacters(in: .whitespaces)) }
            .filter { $0 != 0 && $0 != ownPort }
        guard !ports.isEmpty else { return }
        let body = #"{"change":"\#(change)"}"#
        DispatchQueue.global(qos: .utility).async {
            for port in Set(ports) {
                guard let url = URL(string: "http://127.0.0.1:\(port)/state-changed") else {
                    continue
                }
                var request = URLRequest(url: url)
                request.httpMethod = "POST"
                request.timeoutInterval = 0.25
                request.setValue("application/json", forHTTPHeaderField: "Content-Type")
                request.httpBody = Data(body.utf8)
                URLSession.shared.dataTask(with: request).resume()
            }
        }
    }

    func announceStateChange(_ change: String) {
        Self.announceStateChange(change, ownPort: hookServer?.port)
    }

    @discardableResult
    static func writeSharedProjectOrder(
        siblingIDs: [String], fallbackAllIDs: [String]
    ) -> Bool {
        // Merge under the cross-frontend lock. A project drag and a child
        // drag can happen in different frontends at once; replacing only
        // this sibling set's occupied ranks keeps both edits.
        let wrote = PresetStateFile.withExclusiveLock(on: sharedProjectOrderURL) {
            var shared = (try? Data(contentsOf: sharedProjectOrderURL))
                .flatMap { try? JSONSerialization.jsonObject(with: $0) } as? [String]
                ?? fallbackAllIDs
            for id in fallbackAllIDs where !shared.contains(id) { shared.append(id) }
            let siblingSet = Set(siblingIDs)
            let slots = shared.indices.filter { siblingSet.contains(shared[$0]) }
            guard slots.count == siblingIDs.count else { return false }
            for (slot, id) in zip(slots, siblingIDs) { shared[slot] = id }
            guard let merged = try? JSONSerialization.data(withJSONObject: shared) else {
                return false
            }
            do {
                try merged.write(to: sharedProjectOrderURL, options: .atomic)
                return true
            } catch {
                return false
            }
        } ?? false
        sharedProjectOrderCache = nil
        return wrote
    }

    private func applyProjectOrderOverlay(_ base: [Project], parentID: String?) -> [Project] {
        let baseIDs = Set(base.map(\.id))
        // An in-flight drag preview outranks everything, then the
        // cross-frontend file when it knows this sibling set. Older files
        // contain top-level ids only, so retain the existing per-parent
        // UserDefaults overlay as a migration fallback for child folders.
        let preview = projectOrderPreview.flatMap {
            $0.parentID == parentID ? $0.ids : nil
        }
        let shared = Self.sharedProjectOrder()?.filter { baseIDs.contains($0) }
        guard let overlay = preview
                ?? (shared?.isEmpty == false ? shared : nil)
                ?? AppDefaults.shared.stringArray(
                    forKey: Self.projectOrderKey(forParent: parentID)
                ),
              !overlay.isEmpty
        else { return base }
        // Unknown ids are skipped, NOT GC'd here: a project can be merely
        // not-yet-known at read time (ephemeral test projects register
        // after the first rescan). Stale ids are dropped when the next
        // drag persists a fresh order (setProjectOrder writes current ids
        // only), so the overlay cannot grow unbounded.
        let known = overlay.filter { baseIDs.contains($0) }
        guard !known.isEmpty else { return base }
        var rank: [String: Int] = [:]
        for (index, id) in known.enumerated() { rank[id] = index }
        let ordered = base.filter { rank[$0.id] != nil }
            .sorted { rank[$0.id]! < rank[$1.id]! }
        let rest = base.filter { rank[$0.id] == nil }
        return ordered + rest
    }

    private func pruneProjectOrderOverlays(removing removedIDs: Set<String>) {
        guard !removedIDs.isEmpty else { return }
        let defaults = AppDefaults.shared
        let childOrderPrefix = Self.projectOrderKey + "."
        let keys = defaults.dictionaryRepresentation().keys.filter {
            $0 == Self.projectOrderKey || $0.hasPrefix(childOrderPrefix)
        }
        for key in keys {
            if key.hasPrefix(childOrderPrefix),
               removedIDs.contains(String(key.dropFirst(childOrderPrefix.count))) {
                defaults.removeObject(forKey: key)
                continue
            }
            guard var ids = defaults.stringArray(forKey: key),
                  ids.contains(where: removedIDs.contains)
            else { continue }
            ids.removeAll { removedIDs.contains($0) }
            if ids.isEmpty {
                defaults.removeObject(forKey: key)
            } else {
                defaults.set(ids, forKey: key)
            }
        }
    }

    private func applySessionOrderOverlay(
        _ base: [SessionEntry], projectID: String
    ) -> [SessionEntry] {
        // Date sort ignores the manual order entirely (the stored order
        // survives for a switch back) and uses the same shape as Recent:
        // working rows first, then every other lifecycle event newest-first.
        // EXCEPT while a drag's live reorder preview is in flight: the
        // preview renders even in a date-sorted list (smooth sort), and the
        // drop either flips the list to custom order (commit) or drops the
        // preview and falls straight back to date order (cancel).
        if dateSortedProjectIDs.contains(projectID),
           sessionOrderPreviews[projectID] == nil {
            return Self.sessionsSortedByRecentActivity(
                base,
                restartingSessionIDs: restartingSessionIDs
            )
        }
        // Same precedence as `sidebarSessionBlocks`: the cross-frontend file
        // wins, the local overlay is the fallback.
        let key = Self.sessionOrderKey(projectID)
        guard let overlay = sessionOrderPreviews[projectID]
                ?? Self.sharedSessionOrder(projectID: projectID)
                ?? AppDefaults.shared.stringArray(forKey: key),
              !overlay.isEmpty
        else { return base }
        let baseIDs = Set(base.map(\.id))
        // Unknown ids are skipped, NOT GC'd at read: a session can vanish
        // from one rescan transiently (torn manifest decode mid-heartbeat).
        // Stale ids drop out when the next drag persists a fresh order
        // (setSessionOrder), and removeSession prunes its id explicitly.
        let known = overlay.filter { baseIDs.contains($0) }
        guard !known.isEmpty else { return base }
        var rank: [String: Int] = [:]
        for (index, id) in known.enumerated() { rank[id] = index }
        // Sessions NOT in the overlay are newer than every overlay entry
        // (the overlay snapshots the whole visible list at drag time), so
        // they keep newest-first order ABOVE the hand-ordered block —
        // preserving "new sessions appear at the top".
        let rest = base.filter { rank[$0.id] == nil }
        let ordered = base.filter { rank[$0.id] != nil }
            .sorted { rank[$0.id]! < rank[$1.id]! }
        return rest + ordered
    }

    /// Pure shared rank for the Recent page shape and per-group "Recently
    /// updated" mode. A live-but-idle session is NOT privileged over a more
    /// recent exited one; only work currently in progress gets the leading
    /// tier. Id is the deterministic final tie-break across rescans/frontends.
    static func sessionsSortedByRecentActivity(
        _ sessions: [SessionEntry],
        restartingSessionIDs: Set<String> = []
    ) -> [SessionEntry] {
        func isWorking(_ session: SessionEntry) -> Bool {
            session.status == .starting
                || session.status == .busy
                || restartingSessionIDs.contains(session.id)
        }
        func stamp(_ session: SessionEntry) -> Int64 {
            max(session.createdAt, session.lifecycleAtMs ?? 0)
        }
        return sessions.sorted { lhs, rhs in
            let lhsWorking = isWorking(lhs)
            let rhsWorking = isWorking(rhs)
            if lhsWorking != rhsWorking { return lhsWorking }
            let lhsStamp = stamp(lhs)
            let rhsStamp = stamp(rhs)
            if lhsStamp != rhsStamp { return lhsStamp > rhsStamp }
            return lhs.id < rhs.id
        }
    }

    /// Durable project/worktree sibling move used by tests and non-drag
    /// callers: move `draggedID` to `targetID`'s position among siblings and
    /// persist that sibling order. The desktop drag path uses
    /// `previewProjectMove` and commits only on drop. Remote projects route
    /// through the Host's `project.organization.set` — never local state.
    func moveProject(draggedID: String, over targetID: String) {
        guard let (parentID, ids) = projectSiblingMove(
            draggedID: draggedID, over: targetID
        ) else { return }
        if routesProjectVerbThroughHost(draggedID) {
            commitRemoteProjectOrder(draggedID: draggedID, ids: ids)
            return
        }
        setProjectOrder(ids, parentID: parentID)
    }

    /// In-memory move used by the desktop drag path. It is applied exactly
    /// once inside the accepted drop's animations-disabled transaction; no
    /// shared/local state is written until `commitProjectReorder`.
    /// Works identically in remote scope: the preview reorders the remote
    /// projection in memory (`projectRemoteScope` reads it) and nothing local
    /// is touched.
    func previewProjectMove(
        draggedID: String,
        targetID: String,
        below: Bool
    ) {
        guard let (parentID, ids) = projectSiblingInsertion(
            draggedID: draggedID,
            targetID: targetID,
            below: below
        ) else { return }
        guard projectOrderPreview?.parentID != parentID
            || projectOrderPreview?.ids != ids
        else { return }
        projectOrderPreview = (parentID, ids, draggedID)
        if routesProjectVerbThroughHost(draggedID) {
            projectRemoteScope(snapshot: remoteHostRuntime.snapshot)
            return
        }
        rebuildTreeFromLastScan()
    }

    /// Persist the final desktop drag preview exactly once. Local scope
    /// writes the shared order files; remote scope commits the SAME drag
    /// through the Host's `project.organization.set` (one-project patch: the
    /// dragged project's final sibling index) and keeps the optimistic order
    /// on screen until the refreshed bootstrap confirms it. No local
    /// project-order state is ever written for remote entities.
    func commitProjectReorder() {
        guard let preview = projectOrderPreview else { return }
        // The displayed tree still carries the live preview. Capture the
        // final visible sibling order before removing its precedence.
        let ids = projectOrderIDs(parentID: preview.parentID)
        projectOrderPreview = nil
        if routesProjectVerbThroughHost(preview.draggedID) {
            commitRemoteProjectOrder(draggedID: preview.draggedID, ids: ids)
            return
        }
        setProjectOrder(ids, parentID: preview.parentID, animated: false)
    }

    /// Remote half of a project reorder: send the dragged project's new
    /// sibling index; the Host applies it to its own display order through
    /// the same choke point a local drag uses, and the runtime's bootstrap
    /// refresh reconciles the projection.
    private func commitRemoteProjectOrder(draggedID: String, ids: [String]) {
        guard let index = ids.firstIndex(of: draggedID) else {
            withAnimation(.easeInOut(duration: 0.18)) {
                projectRemoteScope(snapshot: remoteHostRuntime.snapshot)
            }
            return
        }
        // Hold the dropped order on screen until a bootstrap confirms it —
        // a poll captured before the Host applied the write must not snap
        // the drag back. A failed verb rolls back visibly with its alert.
        remoteCommittedOrderHold =
            (remoteProjectsByID[draggedID]?.parentProjectID, ids, Date())
        let workspaceKey = workspacePoolForegroundKey()
        if let workspaceKey {
            workspacePool.holdProjectOrder(
                forKey: workspaceKey,
                parentID: remoteProjectsByID[draggedID]?.parentProjectID,
                orderedIDs: ids
            )
        }
        performRemoteVerb("Couldn't reorder the projects", onFailure: { [weak self] in
            guard let self else { return }
            self.remoteCommittedOrderHold = nil
            if let workspaceKey {
                self.workspacePool.clearProjectOrderHold(forKey: workspaceKey)
                self.workspacePool.dropSnapshot(forKey: workspaceKey)
            }
            withAnimation(.easeInOut(duration: 0.18)) {
                self.projectRemoteScope(snapshot: self.remoteHostRuntime.snapshot)
            }
        }) { runtime in
            try await runtime.setProjectSortOrder(
                projectID: draggedID,
                sortOrder: index
            )
        }
    }

    /// Roll back a drag that left the sidebar or otherwise did not produce an
    /// accepted drop. The persisted order was never touched.
    func cancelProjectReorder() {
        guard let preview = projectOrderPreview else { return }
        projectOrderPreview = nil
        if routesProjectVerbThroughHost(preview.draggedID) {
            withAnimation(.easeInOut(duration: 0.18)) {
                projectRemoteScope(snapshot: remoteHostRuntime.snapshot)
            }
            return
        }
        withAnimation(.easeInOut(duration: 0.18)) { rebuildTreeFromLastScan() }
    }

    /// Shared sibling-reorder math: the dragged project takes the target's
    /// slot among same-parent siblings (a cross-parent pair is a no-op).
    /// Scope-neutral: reads the displayed tree, so the same drag works over
    /// local nodes and the remote projection alike.
    private func projectSiblingMove(
        draggedID: String, over targetID: String
    ) -> (parentID: String?, ids: [String])? {
        guard draggedID != targetID else { return nil }
        guard let dragged = displayProjectsByID[draggedID],
              let target = displayProjectsByID[targetID],
              dragged.parentProjectID == target.parentProjectID
        else { return nil }
        let parentID = dragged.parentProjectID
        var ids = projectOrderIDs(parentID: parentID)
        guard let from = ids.firstIndex(of: draggedID),
              let to = ids.firstIndex(of: targetID)
        else { return nil }
        ids.remove(at: from)
        ids.insert(draggedID, at: to)
        return (parentID, ids)
    }

    /// Exact gap insertion used by the detached project drag. The visual gap
    /// already resolved above/below from the target block midpoint; committing
    /// that same side prevents a release from replaying into a neighboring
    /// inferred slot when the source crossed the target from either direction.
    private func projectSiblingInsertion(
        draggedID: String,
        targetID: String,
        below: Bool
    ) -> (parentID: String?, ids: [String])? {
        guard draggedID != targetID else { return nil }
        guard let dragged = displayProjectsByID[draggedID],
              let target = displayProjectsByID[targetID],
              dragged.parentProjectID == target.parentProjectID
        else { return nil }
        let parentID = dragged.parentProjectID
        guard let ids = Self.projectInsertionOrder(
            ids: projectOrderIDs(parentID: parentID),
            draggedID: draggedID,
            targetID: targetID,
            below: below
        ) else { return nil }
        return (parentID, ids)
    }

    nonisolated static func projectInsertionOrder(
        ids: [String],
        draggedID: String,
        targetID: String,
        below: Bool
    ) -> [String]? {
        guard draggedID != targetID, ids.contains(draggedID) else { return nil }
        var result = ids
        result.removeAll { $0 == draggedID }
        guard let targetIndex = result.firstIndex(of: targetID) else { return nil }
        result.insert(draggedID, at: targetIndex + (below ? 1 : 0))
        return result
    }

    private func projectOrderIDs(parentID: String?) -> [String] {
        guard let parentID else { return displayNodes.map(\.id) }
        return findDisplayNode(parentID)?.worktrees.map(\.id) ?? []
    }

    private func flattenedProjectOrderIDs() -> [String] {
        var ids: [String] = []
        func append(_ nodes: [ProjectNode]) {
            for node in nodes {
                ids.append(node.id)
                append(node.worktrees)
            }
        }
        append(nodes)
        return ids
    }

    func setProjectOrder(
        _ ids: [String],
        parentID: String? = nil,
        animated: Bool = true
    ) {
        let key = Self.projectOrderKey(forParent: parentID)
        if ids.isEmpty {
            AppDefaults.shared.removeObject(forKey: key)
        } else {
            AppDefaults.shared.set(ids, forKey: key)
        }
        // Publish every sibling reorder. The shared representation is one
        // flat list: replace just this sibling set's occupied slots, leaving
        // every other parent/root rank untouched.
        if !ids.isEmpty {
            if Self.writeSharedProjectOrder(
                siblingIDs: ids,
                fallbackAllIDs: flattenedProjectOrderIDs()
            ) {
                announceStateChange("order")
            }
        }
        if animated {
            withAnimation(.easeInOut(duration: 0.18)) { rebuildTreeFromLastScan() }
        } else {
            var transaction = Transaction()
            transaction.disablesAnimations = true
            withTransaction(transaction) { rebuildTreeFromLastScan() }
        }
    }

    /// Order-overlay-only rebuild for the drag-reorder path: reapplies the
    /// native order overlays over the last scan's inputs. Session previews
    /// fire on every drag-hover tick, and a full rescan (directory scan +
    /// every UserDefaults overlay + activity snapshot) per tick made dragging
    /// lag.
    private func rebuildTreeFromLastScan() {
        guard hasCompletedScan else { return rescan() }
        rebuildTree(projects: lastScanProjects, sessions: lastScanSessions)
        rebuildPins(tauriPins: lastScanTauriPins)
    }

    /// In-memory move used by the desktop drag path. Rows still animate live,
    /// but no shared/local state is written until `commitSessionReorder`.
    func previewSessionMove(projectID: String, draggedID: String, over targetID: String) {
        guard draggedID != targetID,
              !sessionIsRecentArchived(draggedID),
              !sessionIsRecentArchived(targetID),
              let node = findDisplayNode(projectID)
        else { return }
        let pinnedIDs = renderedPinnedItems(in: node).map(\.id)
        var regularIDs = renderedDisplayedItems(in: node).filter { item in
            if case .session(let session) = item {
                return !sessionIsRecentArchived(session.id)
            }
            return true
        }.map(\.id)
        guard let from = regularIDs.firstIndex(of: draggedID),
              let to = regularIDs.firstIndex(of: targetID)
        else { return }
        regularIDs.remove(at: from)
        regularIDs.insert(draggedID, at: to)
        let preview = Self.combinedSessionOrder(
            pinnedIDs: pinnedIDs, regularIDs: regularIDs
        )
        guard sessionOrderPreviews[projectID] != preview else { return }
        sessionOrderPreviews[projectID] = preview
        refreshAfterOrderPreviewChange(projectID: projectID)
    }

    /// Durable regular-section move used by tests and non-drag callers. The
    /// desktop drag path uses `previewSessionMove` and commits only on drop.
    func moveSession(projectID: String, draggedID: String, over targetID: String) {
        guard draggedID != targetID,
              !sessionIsRecentArchived(draggedID),
              !sessionIsRecentArchived(targetID),
              let node = findNode(projectID)
        else { return }
        let pinnedIDs = Set(
            (pinnedByProject[projectID] ?? []).compactMap(\.sessionID)
        )
        var ids = node.sessions.map(\.id).filter {
            !pinnedIDs.contains($0) && !sessionIsRecentArchived($0)
        }
        guard let from = ids.firstIndex(of: draggedID),
              let to = ids.firstIndex(of: targetID)
        else { return }
        ids.remove(at: from)
        ids.insert(draggedID, at: to)
        setSessionOrder(projectID: projectID, ids: ids)
    }

    func setSessionOrder(projectID: String, ids: [String]) {
        let regularIDs = ids.filter { !sessionIsRecentArchived($0) }
        // The pinned bucket of the combined shared list is the MIXED
        // partition (session ids and pinned-group ids), so a regular-section
        // commit must not drop the groups' pin ranks.
        let pinnedIDs = (pinnedByProject[projectID] ?? []).compactMap(\.orderTargetID)
            .filter { !sessionIsRecentArchived($0) }
        let shared = Self.combinedSessionOrder(
            pinnedIDs: pinnedIDs,
            regularIDs: regularIDs
        )
        if routesProjectVerbThroughHost(projectID) {
            performRemoteVerb("Couldn't reorder the sessions") { runtime in
                try await runtime.setSessionOrder(
                    projectID: projectID,
                    orderedSessionIDs: shared
                )
            }
            return
        }
        if Self.writeSharedSessionOrder(projectID: projectID, ids: shared) {
            announceStateChange("order")
        }
        AppDefaults.shared.set(regularIDs, forKey: Self.sessionOrderKey(projectID))
        withAnimation(.easeInOut(duration: 0.18)) { rebuildTreeFromLastScan() }
    }

    /// Pinned-section counterpart to `previewSessionMove`. The pinned
    /// partition is the MIXED list of pinned sessions and pinned child
    /// groups, so either row kind can preview over any pinned sibling.
    func previewPinnedSessionMove(
        projectID: String, draggedID: String, over targetID: String
    ) {
        guard draggedID != targetID, let node = findDisplayNode(projectID) else { return }
        var pinnedIDs = renderedPinnedItems(in: node).map(\.id)
        guard let from = pinnedIDs.firstIndex(of: draggedID),
              let to = pinnedIDs.firstIndex(of: targetID)
        else { return }
        pinnedIDs.remove(at: from)
        pinnedIDs.insert(draggedID, at: to)
        let regularIDs = renderedDisplayedItems(in: node).filter { item in
            if case .session(let session) = item {
                return !sessionIsRecentArchived(session.id)
            }
            return true
        }.map(\.id)
        let preview = Self.combinedSessionOrder(
            pinnedIDs: pinnedIDs, regularIDs: regularIDs
        )
        guard sessionOrderPreviews[projectID] != preview else { return }
        sessionOrderPreviews[projectID] = preview
        refreshAfterOrderPreviewChange(projectID: projectID)
    }

    /// Durable pinned-partition move used by tests and non-drag callers. The
    /// desktop drag path uses `previewPinnedSessionMove`; the partition is
    /// the mixed pinned sessions + pinned child groups list.
    func movePinnedSession(projectID: String, draggedID: String, over targetID: String) {
        guard draggedID != targetID,
              !sessionIsRecentArchived(draggedID),
              !sessionIsRecentArchived(targetID),
              let node = findNode(projectID)
        else { return }
        var ids = renderedPinnedItems(in: node).map(\.id)
        guard let from = ids.firstIndex(of: draggedID),
              let to = ids.firstIndex(of: targetID)
        else { return }
        ids.remove(at: from)
        ids.insert(draggedID, at: to)
        setPinnedOrder(projectID: projectID, ids: ids)
    }

    /// Persist the pinned partition's mixed order (session ids AND pinned
    /// child-group ids): the legacy local overlay, the combined shared rank
    /// list, and the durable `pinned_sessions` records array all receive
    /// the same new order.
    func setPinnedOrder(projectID: String, ids: [String]) {
        let orderableIDs = ids.filter { !sessionIsRecentArchived($0) }
        let pinned = Set(orderableIDs)
        let regularIDs = findNode(projectID).map { node in
            renderedDisplayedItems(in: node).filter { item in
                if case .session(let session) = item {
                    return !sessionIsRecentArchived(session.id)
                }
                return true
            }.map(\.id).filter { !pinned.contains($0) }
        } ?? []
        let shared = Self.combinedSessionOrder(
            pinnedIDs: orderableIDs,
            regularIDs: regularIDs
        )
        if routesProjectVerbThroughHost(projectID) {
            performRemoteVerb("Couldn't reorder the pinned sessions") { runtime in
                try await runtime.setSessionOrder(
                    projectID: projectID,
                    orderedSessionIDs: shared
                )
            }
            return
        }
        AppDefaults.shared.set(orderableIDs, forKey: Self.pinnedOrderKey(projectID))
        if Self.writeSharedSessionOrder(projectID: projectID, ids: shared) {
            announceStateChange("order")
        }
        rewritePinnedRecordOrder(projectID: projectID, orderedTargetIDs: orderableIDs)
        withAnimation(.easeInOut(duration: 0.18)) { rebuildTreeFromLastScan() }
    }

    /// Rewrite the project's `pinned_sessions` records array to the given
    /// mixed target order — same locked + announced app-state write the pin
    /// verbs use. Each record keeps its key, pinned_at, and unknown fields;
    /// only the array order changes. A ranked pinned group that has no
    /// record yet (pinned from the TUI, marker only) gains its ordering
    /// record here so the dragged position is durable.
    private func rewritePinnedRecordOrder(
        projectID: String, orderedTargetIDs: [String]
    ) {
        guard !orderedTargetIDs.isEmpty else { return }
        let groupIDs = Set(orderedTargetIDs.filter {
            projectsByID[$0]?.acceptsSessionDrop == true
        })
        let wrote = editPresetStateAnnouncing { object in
            Self.applyPinnedRecordOrder(
                orderedTargetIDs,
                projectID: projectID,
                groupIDs: groupIDs,
                newRecordStamp: Self.nextPinIntentTimestamp(),
                to: &object
            )
        }
        guard wrote else { return }
        // Keep the in-memory scan snapshot aligned so the very next
        // `rebuildTreeFromLastScan` already ranks any synthesized group
        // record instead of waiting for the FSEvents rescan.
        lastScanTauriPins = loadAppState()?.pinnedSessions ?? [:]
    }

    /// Raw-JSON reorder of one project's `pinned_sessions` rows, used
    /// inside the app-state lock. Rows not named by the order (for
    /// example archived pinned sessions, whose durable pin must survive)
    /// keep their relative order below the ranked block. Unknown shapes
    /// return false and leave the object untouched.
    @discardableResult
    static func applyPinnedRecordOrder(
        _ orderedTargetIDs: [String],
        projectID: String,
        groupIDs: Set<String>,
        newRecordStamp: UInt64,
        to object: inout [String: Any]
    ) -> Bool {
        guard !orderedTargetIDs.isEmpty else { return true }
        let rawPins = object["pinned_sessions"]
        var grouped: [String: [[String: Any]]] = [:]
        if rawPins == nil || rawPins is NSNull {
            grouped = [:]
        } else if let rawGroups = rawPins as? [String: Any] {
            for (project, rawRows) in rawGroups {
                guard let rows = rawRows as? [Any],
                      rows.allSatisfy({ $0 is [String: Any] })
                else { return false }
                grouped[project] = rows.compactMap { $0 as? [String: Any] }
            }
        } else if let rawRows = rawPins as? [Any] {
            // Legacy app-state.json stored one flat array.
            for rawRow in rawRows {
                guard let row = rawRow as? [String: Any],
                      let project = row["project_id"] as? String
                else { return false }
                grouped[project, default: []].append(row)
            }
        } else {
            return false
        }

        func rowTargetID(_ row: [String: Any]) -> String? {
            if let sessionID = row["session_id"] as? String { return sessionID }
            guard let key = row["key"] as? String else { return nil }
            if key.hasPrefix("project:") {
                return String(key.dropFirst("project:".count))
            }
            if key.hasPrefix("session:") {
                return String(key.dropFirst("session:".count))
            }
            return nil
        }

        var rank: [String: Int] = [:]
        for (index, id) in orderedTargetIDs.enumerated() where rank[id] == nil {
            rank[id] = index
        }
        var rankedRows: [(rank: Int, row: [String: Any])] = []
        var rest: [[String: Any]] = []
        var placedTargets = Set<String>()
        for row in grouped[projectID] ?? [] {
            if let target = rowTargetID(row), let index = rank[target],
               placedTargets.insert(target).inserted {
                rankedRows.append((index, row))
            } else {
                rest.append(row)
            }
        }
        for id in orderedTargetIDs
        where groupIDs.contains(id) && !placedTargets.contains(id) {
            guard let index = rank[id] else { continue }
            placedTargets.insert(id)
            rankedRows.append((index, [
                "key": PinnedSidebarSession.key(forProjectID: id),
                "project_id": projectID,
                "pinned_at": newRecordStamp,
            ]))
        }
        guard !rankedRows.isEmpty else { return true }
        let ordered = rankedRows.sorted { $0.rank < $1.rank }.map(\.row)
        if ordered.isEmpty && rest.isEmpty {
            grouped.removeValue(forKey: projectID)
        } else {
            grouped[projectID] = ordered + rest
        }
        object["pinned_sessions"] = grouped
        return true
    }

    /// Persist the final desktop drag preview exactly once. The section flag
    /// chooses which legacy UserDefaults fallback is kept in sync; the shared
    /// file always receives the combined pin + regular order.
    func commitSessionReorder(projectID: String, pinned: Bool) {
        guard sessionOrderPreviews[projectID] != nil else { return }
        // Remote nodes commit the combined pinned + regular visible order
        // through the Host's `session.order.set`, exactly as a desktop drag
        // commits it locally. The optimistic order keeps the rows in place
        // until the next bootstrap confirms it.
        if routesProjectVerbThroughHost(projectID) {
            let wasDateSorted = isDateSorted(projectID: projectID)
            guard let node = findDisplayNode(projectID) else {
                sessionOrderPreviews.removeValue(forKey: projectID)
                return
            }
            let orderedIDs = (renderedPinnedSessions(in: node)
                + renderedDisplayedSessions(in: node))
                .filter { !sessionIsRecentArchived($0.id) }
                .map(\.id)
            remoteSessionOrderByProject[projectID] = orderedIDs
            let workspaceKey = workspacePoolForegroundKey()
            remoteCommittedSessionOrderHolds[projectID] =
                RemoteCommittedSessionOrderHold(
                    workspaceKey: workspaceKey,
                    ids: orderedIDs,
                    heldAt: Date()
                )
            if let workspaceKey {
                workspacePool.holdSessionOrder(
                    forKey: workspaceKey,
                    projectID: projectID,
                    orderedIDs: orderedIDs
                )
            }
            sessionOrderPreviews.removeValue(forKey: projectID)
            invalidateSidebarLists()
            performRemoteVerb("Couldn't reorder the sessions", onFailure: { [weak self] in
                guard let self else { return }
                self.remoteCommittedSessionOrderHolds.removeValue(forKey: projectID)
                if let workspaceKey {
                    self.workspacePool.clearSessionOrderHold(
                        forKey: workspaceKey,
                        projectID: projectID
                    )
                    self.workspacePool.dropSnapshot(forKey: workspaceKey)
                }
                self.projectRemoteScope(snapshot: self.remoteHostRuntime.snapshot)
            }) { runtime in
                if wasDateSorted {
                    try await runtime.setProjectDateSorted(
                        projectID: projectID,
                        dateSorted: false
                    )
                }
                try await runtime.setSessionOrder(
                    projectID: projectID,
                    orderedSessionIDs: orderedIDs
                )
            }
            return
        }
        // `nodes` and `pinnedByProject` still carry the live preview. Capture
        // those final visible orders before removing its precedence. The
        // pinned partition is the mixed pinned sessions + pinned child
        // groups list, in exactly the order the preview shows.
        let pinnedIDs = findNode(projectID).map {
            renderedPinnedItems(in: $0).map(\.id)
        } ?? []
        let regularIDs = findNode(projectID).map {
            renderedDisplayedItems(in: $0).filter { item in
                if case .session(let session) = item {
                    return !sessionIsRecentArchived(session.id)
                }
                return true
            }.map(\.id)
        } ?? []
        sessionOrderPreviews.removeValue(forKey: projectID)
        if pinned {
            setPinnedOrder(projectID: projectID, ids: pinnedIDs)
        } else {
            // Hand-ordering a date-sorted list IS choosing custom order:
            // without the flip the committed order would snap back to date
            // order on the next rescan. Pinned-section commits never flip —
            // the pinned order is manual regardless of the list's sort mode.
            if dateSortedProjectIDs.contains(projectID) {
                setSessionDateSorted(false, for: projectID)
            }
            setSessionOrder(projectID: projectID, ids: regularIDs)
        }
    }

    /// Roll back a drag that left the sidebar or otherwise did not produce an
    /// accepted drop. The persisted order was never touched.
    func cancelSessionReorder(projectID: String) {
        guard sessionOrderPreviews.removeValue(forKey: projectID) != nil else { return }
        if routesProjectVerbThroughHost(projectID) {
            withAnimation(.easeInOut(duration: 0.18)) {
                projectRemoteScope(snapshot: remoteHostRuntime.snapshot)
            }
            return
        }
        withAnimation(.easeInOut(duration: 0.18)) { rebuildTreeFromLastScan() }
    }

    private func findNode(_ projectID: String) -> ProjectNode? {
        func search(_ nodes: [ProjectNode]) -> ProjectNode? {
            for node in nodes {
                if node.id == projectID { return node }
                if let found = search(node.worktrees) { return found }
            }
            return nil
        }
        return search(nodes)
    }

    // MARK: - Rename session (rename_session, pty_manager.rs:2197-2215)

    enum SessionTitleEditorSurface: Equatable {
        case sidebar
        case paneHeader
    }

    /// Identifies which of the two mirrored title surfaces owns first
    /// responder, so starting a pane-header rename does not also mount the
    /// sidebar row's TextField for the same Session.
    @Published private(set) var editingSessionSurface: SessionTitleEditorSurface = .sidebar

    /// Session id whose active title surface shows the inline rename editor
    /// (one at a time, like `editingSessionId` in ProjectItem.svelte:146).
    @Published var editingSessionID: String? {
        didSet {
            if editingSessionID == nil {
                editingSessionSurface = .sidebar
                scheduleDeferredRemoteProjectionFlush()
            }
        }
    }

    func beginEditingSessionTitle(
        _ sessionID: String,
        on surface: SessionTitleEditorSurface
    ) {
        editingSessionSurface = surface
        editingSessionID = sessionID
    }

    /// Legacy native rename fallback: [session id: custom title]. New writes
    /// publish `title.json`; pre-marker values are migrated there on scan and
    /// retained here only if that shared write fails. We also mirror the
    /// resolved title into manifest.json so the Rust host and Sessions MCP
    /// report the same title as the sidebar.
    private func loadSessionTitleOverrides() -> [String: String] {
        (AppDefaults.shared.dictionary(forKey: NativeOverlay.sessionTitlesKey)
            as? [String: String]) ?? [:]
    }

    private func saveSessionTitleOverrides(_ overrides: [String: String]) {
        if overrides.isEmpty {
            AppDefaults.shared.removeObject(forKey: NativeOverlay.sessionTitlesKey)
        } else {
            AppDefaults.shared.set(overrides, forKey: NativeOverlay.sessionTitlesKey)
        }
    }

    static func decodedPendingTitleWrites(_ stored: Any?) -> [String: UInt64] {
        if let data = stored as? Data,
           let decoded = try? JSONDecoder().decode([String: UInt64].self, from: data) {
            return decoded
        }
        if let dictionary = stored as? [String: Any] {
            return dictionary.reduce(into: [:]) { result, item in
                if let timestamp = jsonUInt64(item.value) {
                    result[item.key] = timestamp
                }
            }
        }
        if let legacySessionIDs = stored as? [String] {
            // The first uncommitted implementation stored only a pending bit.
            // Zero means "unknown age": it may retry when no marker exists,
            // but it can never overwrite a valid shared marker.
            return Dictionary(
                legacySessionIDs.map { ($0, UInt64(0)) },
                uniquingKeysWith: { _, newest in newest }
            )
        }
        return [:]
    }

    private func loadPendingTitleWrites() -> [String: UInt64] {
        Self.decodedPendingTitleWrites(
            AppDefaults.shared.object(forKey: Self.nativePendingTitleWritesKey)
        )
    }

    private func savePendingTitleWrites(_ writes: [String: UInt64]) {
        if writes.isEmpty {
            AppDefaults.shared.removeObject(forKey: Self.nativePendingTitleWritesKey)
        } else if let data = try? JSONEncoder().encode(writes) {
            AppDefaults.shared.set(data, forKey: Self.nativePendingTitleWritesKey)
        }
    }

    static func normalizedSessionTitle(_ title: String?) -> String? {
        let normalized = title?.trimmingCharacters(in: .whitespacesAndNewlines) ?? ""
        return normalized.isEmpty ? nil : normalized
    }

    struct SessionTitleResolution: Equatable {
        let title: String?
        let shouldPublishNative: Bool
    }

    /// Shared markers are the durable App/TUI contract. A failed native write
    /// retries only when its durable intent timestamp is newer. Timestamp-less
    /// legacy pending bits defer to any valid marker, preventing an old native
    /// fallback from overwriting a later TUI/CLI rename after relaunch.
    static func resolvedSessionTitle(
        sharedMarker: SharedTitleMarker?,
        nativeTitle: String?,
        pendingWriteAt: UInt64?
    ) -> SessionTitleResolution {
        let sharedTitle = normalizedSessionTitle(sharedMarker?.title)
        let nativeTitle = normalizedSessionTitle(nativeTitle)
        guard let nativeTitle else {
            return SessionTitleResolution(
                title: sharedTitle,
                shouldPublishNative: false
            )
        }
        guard let sharedTitle else {
            return SessionTitleResolution(
                title: nativeTitle,
                shouldPublishNative: true
            )
        }
        guard let pendingWriteAt,
              pendingWriteAt > 0,
              let sharedUpdatedAt = sharedMarker?.updatedAt,
              pendingWriteAt > sharedUpdatedAt
        else {
            return SessionTitleResolution(
                title: sharedTitle,
                shouldPublishNative: false
            )
        }
        return SessionTitleResolution(
            title: nativeTitle,
            shouldPublishNative: true
        )
    }

    private static func nextTitleIntentTimestamp(
        after previous: UInt64? = nil
    ) -> UInt64 {
        let now = UInt64(Date().timeIntervalSince1970 * 1_000)
        guard let previous, previous >= now else { return now }
        return previous == UInt64.max ? previous : previous + 1
    }

    /// All native title publication goes through this helper so a successful
    /// atomic replacement cannot be hidden by a same-stamp/same-size cache hit
    /// during the immediate rescan.
    @discardableResult
    private func publishTitleMarker(
        sessionID: String,
        title: String,
        updatedAt: UInt64
    ) -> Bool {
        let wrote = Self.writeSharedMarker(
            sessionID,
            .title,
            ["title": title, "updated_at": updatedAt]
        )
        if wrote {
            titleMarkerCache.removeValue(forKey: sessionID)
        }
        return wrote
    }

    private func syncSessionTitleOverrideToManifest(sessionID: String, label: String) {
        // The Host commits the in-place runtime generation with its own
        // whole-manifest update. Do not race that commit with this legacy
        // compatibility mirror; title.json remains authoritative meanwhile.
        guard !resumingAgentSessionIDs.contains(sessionID) else { return }
        writeSessionManifestFields(sessionID: sessionID) { session in
            var changed = false
            if session["label"] as? String != label {
                session["label"] = label
                changed = true
            }
            if session["custom_title"] as? Bool != true {
                session["custom_title"] = true
                changed = true
            }
            return changed
        }
    }

    @discardableResult
    private func writeSessionManifestFields(
        sessionID: String,
        mutate: (inout [String: Any]) -> Bool
    ) -> Bool {
        Self.withSessionManifestLock(sessionID: sessionID) {
            let manifestURL = LaunchConfig.appSessionsDir
                .appendingPathComponent(sessionID)
                .appendingPathComponent("manifest.json")
            guard let data = try? Data(contentsOf: manifestURL),
                  var object = (try? JSONSerialization.jsonObject(with: data))
                    as? [String: Any],
                  var session = object["session"] as? [String: Any]
            else { return false }

            guard mutate(&session) else { return false }
            object["session"] = session
            guard JSONSerialization.isValidJSONObject(object),
                  let encoded = try? JSONSerialization.data(
                    withJSONObject: object,
                    options: [.prettyPrinted]
                  )
            else { return false }

            do {
                try encoded.write(to: manifestURL, options: [.atomic])
                scanCollector.removeCachedManifest(sessionID)
                return true
            } catch {
                NSLog("[UnpeelNative] failed to sync session title to manifest: \(error)")
                return false
            }
        } ?? false
    }

    /// Cross-process counterpart of Rust `manifest_lock_target` plus
    /// `app_state::lock_exclusive`: both sides flock the exact stable
    /// `~/.unpeel/session-manifest-locks/<sha256(session id)>.lock` path
    /// around every manifest read-modify-write cycle.
    private static func withSessionManifestLock<Result>(
        sessionID: String,
        _ operation: () -> Result
    ) -> Result? {
        let directory = LaunchConfig.unpeelDir
            .appendingPathComponent("session-manifest-locks", isDirectory: true)
        do {
            try FileManager.default.createDirectory(
                at: directory,
                withIntermediateDirectories: true
            )
        } catch {
            NSLog("[UnpeelNative] failed to create manifest lock directory: \(error)")
            return nil
        }
        let digest = SHA256.hash(data: Data(sessionID.utf8))
            .map { String(format: "%02x", $0) }
            .joined()
        let lockURL = directory.appendingPathComponent("\(digest).lock")
        let descriptor = open(
            lockURL.path,
            O_CREAT | O_RDWR | O_CLOEXEC,
            mode_t(0o600)
        )
        guard descriptor >= 0 else { return nil }
        defer { close(descriptor) }
        guard fchmod(descriptor, mode_t(0o600)) == 0,
              flock(descriptor, LOCK_EX) == 0
        else { return nil }
        defer { _ = flock(descriptor, LOCK_UN) }
        return operation()
    }

    // MARK: - Provider conversation ids (resume-on-restart)

    private func loadProviderSessionIDs() -> [String: String] {
        (AppDefaults.shared.dictionary(forKey: NativeOverlay.providerSessionIDsKey)
            as? [String: String]) ?? [:]
    }

    private func saveProviderSessionIDs(_ map: [String: String]) {
        if map.isEmpty {
            AppDefaults.shared.removeObject(forKey: NativeOverlay.providerSessionIDsKey)
        } else {
            AppDefaults.shared.set(map, forKey: NativeOverlay.providerSessionIDsKey)
        }
    }

    private func loadRestartRecommendationDismissals() -> [String: String] {
        (AppDefaults.shared.dictionary(
            forKey: NativeOverlay.restartRecommendationDismissalsKey
        ) as? [String: String]) ?? [:]
    }

    private func saveRestartRecommendationDismissals(_ map: [String: String]) {
        if map.isEmpty {
            AppDefaults.shared.removeObject(
                forKey: NativeOverlay.restartRecommendationDismissalsKey
            )
        } else {
            AppDefaults.shared.set(
                map,
                forKey: NativeOverlay.restartRecommendationDismissalsKey
            )
        }
    }

    // MARK: - Phone resize override (temporary phone-driven terminal size)

    /// Letterbox a session's desktop terminal to a phone's grid. The pane
    /// resize flows through the normal surface→attach path, so the hosted
    /// PTY follows without extra socket traffic.
    @discardableResult
    func setPhoneResizeOverride(sessionID: String, cols: Int, rows: Int) -> Bool {
        guard sessionsByID[sessionID] != nil else { return false }
        let grid = PhoneResizeOverride(
            cols: max(2, min(cols, 300)),
            rows: max(2, min(rows, 120))
        )
        if phoneResizeOverrides[sessionID] != grid {
            phoneResizeOverrides[sessionID] = grid
        }
        return true
    }

    /// Revert a phone-letterboxed session to its natural full-pane size.
    /// On the Host-service client path the fit is Host-published, so the
    /// Mac also sends the phone's `clear` verb — otherwise the next snapshot
    /// would letterbox the pane right back — and ignores the published grid
    /// for that Session until the Host reports it gone.
    func clearPhoneResizeOverride(for sessionID: String) {
        guard phoneResizeOverrides[sessionID] != nil else { return }
        phoneResizeOverrides.removeValue(forKey: sessionID)
        guard localHostClientStarted, selectedHostScope == .local else { return }
        phoneFitLocallyClearedSessionIDs.insert(sessionID)
        Task { @MainActor [weak self] in
            guard let self else { return }
            do {
                try await self.remoteHostRuntime.clearPhoneFit(sessionID: sessionID)
            } catch {
                // The Host still owns the grid; let the next snapshot re-apply
                // it rather than leave the pane and PTY disagreeing.
                self.phoneFitLocallyClearedSessionIDs.remove(sessionID)
                self.remoteHostRuntime.requestImmediateRefresh()
            }
        }
    }

    /// Sessions whose Host-published phone fit this window cleared and is
    /// waiting to see disappear from a snapshot.
    private var phoneFitLocallyClearedSessionIDs: Set<String> = []

    /// Pure derivation of the desktop letterbox overrides from Host-published
    /// session summaries: every running Session carrying a phone grid gets
    /// one, except those this window just cleared (until the Host stops
    /// publishing the grid, which also ends the local clear). Returns the
    /// next override map and the surviving locally-cleared set.
    nonisolated static func phoneResizeOverrides(
        fromHostPublished summaries: [RemoteSessionSummary],
        locallyCleared: Set<String>
    ) -> (overrides: [String: PhoneResizeOverride], locallyCleared: Set<String>) {
        var overrides: [String: PhoneResizeOverride] = [:]
        var survivingClears: Set<String> = []
        for summary in summaries {
            guard summary.status == .running,
                  let cols = summary.phoneFitColumns,
                  let rows = summary.phoneFitRows
            else { continue }
            if locallyCleared.contains(summary.id) {
                survivingClears.insert(summary.id)
                continue
            }
            overrides[summary.id] = PhoneResizeOverride(
                cols: max(2, min(cols, 300)),
                rows: max(2, min(rows, 120))
            )
        }
        return (overrides, survivingClears)
    }

    /// Apply the Host-published phone fits to this window's own Local scope
    /// (the compatibility path keeps `applyRemoteDesktopResize`). Equality-
    /// gated so an unchanged snapshot never republishes the override map.
    private func applyHostPublishedPhoneFits(_ summaries: [RemoteSessionSummary]) {
        let next = Self.phoneResizeOverrides(
            fromHostPublished: summaries,
            locallyCleared: phoneFitLocallyClearedSessionIDs
        )
        if next.locallyCleared != phoneFitLocallyClearedSessionIDs {
            phoneFitLocallyClearedSessionIDs = next.locallyCleared
        }
        if next.overrides != phoneResizeOverrides {
            phoneResizeOverrides = next.overrides
        }
    }

    private func shellQuote(_ value: String) -> String {
        "'" + value.replacingOccurrences(of: "'", with: "'\\''") + "'"
    }

    /// Mirrors ContentArea's actual mount projection closely enough to pick
    /// exactly one resize owner. Zoomed groups mount only the zoomed leaf;
    /// the project sidebar mounts its sessions only while the panel is open.
    private func terminalSurfaceIsMounted(for sessionID: String) -> Bool {
        guard selectedHostScope == .local,
              !settingsVisible,
              archivedProjectID == nil,
              !recentActivityVisible
        else { return false }

        if projectSidebarSessions.contains(where: { $0.id == sessionID }) {
            return true
        }

        guard let selectedSessionID,
              !sessionIsInProjectSidebar(selectedSessionID)
        else { return false }
        guard let group = validatedPaneGroup(containingSession: selectedSessionID) else {
            return selectedSessionID == sessionID
        }
        if let zoomed = zoomedTerminalPane, zoomed.groupID == group.id {
            return group.panes.first(where: { $0.id == zoomed.paneID })?
                .content.sessionID == sessionID
        }
        return group.sessionIDs.contains(sessionID)
    }

    func dismissRestartRecommendation(for sessionID: String) {
        guard let recommendation = restartRecommendations[sessionID] else { return }
        var dismissals = loadRestartRecommendationDismissals()
        dismissals[sessionID] = recommendation.token
        saveRestartRecommendationDismissals(dismissals)
        restartRecommendations.removeValue(forKey: sessionID)
    }

    /// Restart recommendation for a live session, if any. An old hosted PTY
    /// must be replaced to gain current terminal behavior. Launch-context
    /// changes wait for the active agent to end, then offer Resume Agent from
    /// the returned shell. Only a known Host below the essential maintenance
    /// compatibility floor requires a terminal reload.
    static func restartRecommendation(
        for manifest: HostedSessionManifest
    ) -> SessionRestartRecommendation? {
        if let version = manifest.hostProtocolVersion,
           version < requiredSessionHostProtocolVersion {
            return SessionRestartRecommendation(
                token: "host-protocol:\(requiredSessionHostProtocolVersion)",
                message: "Reload to use the updated terminal host.",
                action: .reloadTerminal
            )
        }
        return nil
    }

    /// Record the provider's own conversation metadata for a session (latest
    /// wins — Claude/Codex keep `session_id` stable across a conversation, and
    /// a fresh id after reset is exactly what a later restart/transcript read
    /// should target).
    private func recordProviderMetadata(
        providerSessionID: String?,
        providerTranscriptPath: String?,
        for sessionID: String
    ) {
        var providerIDChanged = false
        if let providerID = providerSessionID {
            var map = loadProviderSessionIDs()
            if map[sessionID] != providerID {
                map[sessionID] = providerID
                saveProviderSessionIDs(map)
                providerIDChanged = true
            }
        }

        writeProviderMetadataToManifest(
            sessionID: sessionID,
            providerSessionID: providerSessionID,
            providerTranscriptPath: providerTranscriptPath
        )

        // Shared marker: the cross-frontend copy (the overlay above is
        // app-only, the manifest write races the host). Merge so an id-only
        // event never erases a captured transcript path; skip unchanged —
        // hooks fire constantly. No state-bus announce, deliberately: every
        // frontend already heard this hook on the same port broadcast.
        let current = Self.readSharedMarker(sessionID, .providerSession) ?? [:]
        var next = current
        if let providerSessionID {
            next["provider_session_id"] = providerSessionID
        }
        if let providerTranscriptPath {
            next["provider_transcript_path"] = providerTranscriptPath
        }
        let changed = (next["provider_session_id"] as? String)
            != (current["provider_session_id"] as? String)
            || (next["provider_transcript_path"] as? String)
            != (current["provider_transcript_path"] as? String)
        if changed {
            next["captured_at"] = Int64(Date().timeIntervalSince1970 * 1000)
            Self.writeSharedMarker(sessionID, .providerSession, next)
        }

        // The conversation identity moved (in-tool /resume or /clear): if the
        // session is still untitled, title it from the resumed conversation's
        // transcript. After the marker/manifest writes above so the host verb
        // resolves the transcript the capture just pointed at; the host
        // no-ops once titling is settled, so a stale-id → fresh-id flip on an
        // already-titled session costs one short-lived process and nothing
        // else. The label lands via the ordinary manifest rescan.
        if providerIDChanged {
            Self.autoTitleFromProviderTranscript(sessionID: sessionID)
        }
    }

    /// Fire-and-forget `unpeel-host __auto_title__ <id>`
    /// (`transcripts::auto_title_session_from_transcript`): titles an
    /// untitled session from its provider conversation — Claude's summary
    /// record or the conversation's first user prompt.
    private nonisolated static func autoTitleFromProviderTranscript(sessionID: String) {
        let process = Process()
        process.executableURL = URL(fileURLWithPath: LaunchConfig.hostBinary)
        process.arguments = ["__auto_title__", sessionID]
        process.standardInput = FileHandle.nullDevice
        process.standardOutput = FileHandle.nullDevice
        process.standardError = FileHandle.nullDevice
        do {
            try process.run()
        } catch {
            NSLog("[UnpeelNative] failed to spawn auto-title: \(error)")
        }
    }

    private func writeProviderMetadataToManifest(
        sessionID: String,
        providerSessionID: String?,
        providerTranscriptPath: String?
    ) {
        _ = Self.withSessionManifestLock(sessionID: sessionID) {
            let manifestURL = LaunchConfig.appSessionsDir
                .appendingPathComponent(sessionID)
                .appendingPathComponent("manifest.json")
            guard let data = try? Data(contentsOf: manifestURL),
                  var object = (try? JSONSerialization.jsonObject(with: data))
                    as? [String: Any]
            else { return }

            var changed = false
            if let providerSessionID,
               object["provider_session_id"] as? String != providerSessionID {
                object["provider_session_id"] = providerSessionID
                changed = true
            }
            if let providerTranscriptPath,
               object["provider_transcript_path"] as? String != providerTranscriptPath {
                object["provider_transcript_path"] = providerTranscriptPath
                changed = true
            }
            guard changed,
                  JSONSerialization.isValidJSONObject(object),
                  let encoded = try? JSONSerialization.data(
                    withJSONObject: object,
                    options: [.prettyPrinted]
                  )
            else { return }
            try? encoded.write(to: manifestURL, options: [.atomic])
            scanCollector.removeCachedManifest(sessionID)
        }
    }

    /// rename_session parity: the overlay entry is the native stand-in for
    /// `custom_title = true` — once set, the backend's auto-titling of the
    /// manifest label never shows again for this session. Empty labels are
    /// rejected (the view reverts to the original instead, matching
    /// commitEdit in ProjectItem.svelte:958-966).
    func renameSession(_ sessionID: String, to label: String) {
        let trimmed = label.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !trimmed.isEmpty else { return }
        if routesSessionVerbThroughHost(sessionID) {
            performRemoteVerb("Couldn't rename the session") { runtime in
                try await runtime.renameSession(sessionID, to: trimmed)
            }
            return
        }
        var overrides = loadSessionTitleOverrides()
        overrides[sessionID] = trimmed
        saveSessionTitleOverrides(overrides)
        var pendingTitleWrites = loadPendingTitleWrites()
        let sharedUpdatedAt = Self.readSharedMarker(sessionID, .title)
            .flatMap { Self.jsonUInt64($0["updated_at"]) }
        let previousTimestamp = max(
            pendingTitleWrites[sessionID] ?? 0,
            sharedUpdatedAt ?? 0
        )
        let intentAt = Self.nextTitleIntentTimestamp(after: previousTimestamp)
        pendingTitleWrites[sessionID] = intentAt
        savePendingTitleWrites(pendingTitleWrites)
        syncSessionTitleOverrideToManifest(sessionID: sessionID, label: trimmed)
        // Save the local fallback before publishing. A successful shared
        // marker becomes authoritative on the rescan below; on failure the
        // pending bit keeps this title recoverable and schedules a retry.
        if publishTitleMarker(
            sessionID: sessionID,
            title: trimmed,
            updatedAt: intentAt
        ) {
            // The durable marker is now the only title authority. Clearing the
            // fallback here also covers a rescan that temporarily cannot decode
            // the session manifest.
            overrides.removeValue(forKey: sessionID)
            saveSessionTitleOverrides(overrides)
            pendingTitleWrites.removeValue(forKey: sessionID)
            savePendingTitleWrites(pendingTitleWrites)
        }
        announceStateChange("session-markers")
        rescan()
    }

    // MARK: - Remove session (kill_session, pty_manager.rs:1701-1753)

    /// Session id whose sidebar row is showing the inline "Remove session?"
    /// confirm state (one at a time, like confirmingArchiveId in
    /// ProjectItem.svelte:158/1009-1024 — the native version swaps the whole
    /// row instead of just the button).
    @Published var confirmingRemoveSessionID: String? {
        didSet {
            if confirmingRemoveSessionID == nil {
                scheduleDeferredRemoteProjectionFlush()
            }
        }
    }

    /// Which surface asked for the pending remove-confirm. The sidebar row
    /// and the archive-page card share `confirmingRemoveSessionID`, but only
    /// the requesting surface may render the inline confirm — the confirm UI
    /// mounts a click-away dismiss monitor scoped to its own row, so a
    /// mirrored confirm on the *other* surface cancels the whole thing on
    /// the very mouse-down aimed at this surface's Delete button (that made
    /// every archive-page delete a no-op until 2026-08-06).
    enum RemoveConfirmSurface { case sidebar, archivePage }
    @Published private(set) var confirmingRemoveSurface: RemoveConfirmSurface = .sidebar

    /// Sessions whose kill/cleanup is in flight; rows render disabled.
    @Published private(set) var removingSessionIDs: Set<String> = []

    /// Session dirs deleted by an explicit close/restart, keyed by session id.
    /// A host from an older build writes its final exited manifest up to a
    /// full heartbeat interval (60s) after its child dies — recreating the
    /// dir we deleted — so a closed session could reappear as a stopped row.
    /// scanSessions deletes a tombstoned dir on sight instead of listing it.
    /// (New hosts skip the final write when the dir is gone; this covers
    /// sessions still running an old `unpeel-host`.)
    private var purgedSessionDirs: [String: Date] = [:]
    private static let purgedSessionDirTTL: TimeInterval = 180

    func requestRemoveSession(
        _ sessionID: String, from surface: RemoveConfirmSurface = .sidebar
    ) {
        // A dead row with no resumable conversation is just clutter — Remove
        // clears it instantly. The confirm exists for losing something: a
        // live terminal, or a conversation that could still resume. Archive
        // rows keep it — those were deliberately kept.
        if surface == .sidebar, !sessionRemovalNeedsConfirmation(sessionID) {
            confirmRemoveSession(sessionID)
            return
        }
        confirmingArchiveSessionID = nil
        confirmingRemoveSurface = surface
        confirmingRemoveSessionID = sessionID
    }

    /// Whether removing this Session deserves a confirmation dialog: it is
    /// live (Remove is also the stop verb) or its conversation could still
    /// be resumed. Unknown ids fail toward confirming.
    func sessionRemovalNeedsConfirmation(_ sessionID: String) -> Bool {
        if let summary = remoteSummariesByID[sessionID] {
            return summary.status == .running
                || summary.capabilities?.archive == true
        }
        guard let session = sessionsByID[sessionID] else { return true }
        return session.isLive || ProviderCapabilities.canArchive(session: session)
    }

    func cancelRemoveConfirm() {
        confirmingRemoveSessionID = nil
    }

    /// Full removal: kill the host via its control socket ({"type":"kill"} →
    /// SIGTERM to the child's process group, session_host.rs:2043-2052), wait
    /// ≤2s for the host to stop, SIGKILL the host pid's group if it is still
    /// alive (spawn_hosted_session_cleanup parity, pty_manager.rs:1117-1132),
    /// then delete the session dir (cleanup_session_artifacts) and prune
    /// every native trace (pins / order overlays / selection / unread).
    /// Dead sessions skip the kill and just clean up.
    func confirmRemoveSession(_ sessionID: String) {
        if confirmingRemoveSessionID == sessionID {
            confirmingRemoveSessionID = nil
        }
        performRemoteVerb("Couldn't remove the session") { runtime in
            try await runtime.removeSession(sessionID)
        }
    }

    // MARK: - Pid identity (anti-recycling guard)

    /// Whether a manifest's recorded pid still refers to the session's own
    /// child process, or the OS has recycled the pid since the child died.
    /// Under agent load the pid counter wraps in well under an hour, so a
    /// stale manifest's pid routinely points at an unrelated live process —
    /// signaling it kills an innocent process group (mirrors PidIdentity in
    /// session_host.rs).
    enum ManifestPidIdentity {
        /// Positively verified: the live process is the recorded child.
        case matches
        /// Positively refuted: the pid was recycled onto an unrelated
        /// process. Treat the session as already dead; never signal.
        case notOurs
        /// Cannot prove either way (legacy manifest without
        /// `pid_started_at` whose child has exec'd away the identifying
        /// argv). Safe default: never force-kill, never declare dead.
        case unknown
    }

    /// Tolerance when comparing the manifest's recorded pid start time
    /// against the kernel-reported one (PID_START_TOLERANCE_MS parity).
    private nonisolated static let pidStartToleranceMs: UInt64 = 10_000

    nonisolated static func processStartTimeMs(_ pid: Int32) -> UInt64? {
        var info = proc_bsdinfo()
        let size = Int32(MemoryLayout<proc_bsdinfo>.stride)
        guard proc_pidinfo(pid, PROC_PIDTBSDINFO, 0, &info, size) == size else { return nil }
        return info.pbi_start_tvsec * 1000 + info.pbi_start_tvusec / 1000
    }

    /// Definitive refutation only: true when the live process at the
    /// manifest's pid provably started at a different time than the recorded
    /// child. Cheap (one syscall), safe to poll.
    private nonisolated static func pidProvablyRecycled(
        _ manifest: HostedSessionManifest?
    ) -> Bool {
        guard let pid = manifest?.pid, pid > 1,
              let recorded = manifest?.pidStartedAt,
              let actual = processStartTimeMs(pid)
        else { return false }
        let drift = actual > recorded ? actual - recorded : recorded - actual
        return drift > pidStartToleranceMs
    }

    nonisolated static func manifestPidIdentity(
        _ manifest: HostedSessionManifest?
    ) -> ManifestPidIdentity {
        guard let manifest, let pid = manifest.pid, pid > 1 else { return .unknown }
        if let recorded = manifest.pidStartedAt, let actual = processStartTimeMs(pid) {
            let drift = actual > recorded ? actual - recorded : recorded - actual
            return drift <= pidStartToleranceMs ? .matches : .notOurs
        }
        // Legacy manifests (no recorded start): the hosted child is spawned
        // as `zsh -l -i -c "<script>"` whose script embeds the session id,
        // so a positive argv hit is definitive. A miss is NOT — after the
        // agent exits, the wrapper execs a plain shell whose argv no longer
        // mentions the session.
        if processCommandLine(pid)?.contains(manifest.session.id) == true {
            return .matches
        }
        return .unknown
    }

    /// Full argv via the KERN_PROCARGS2 sysctl — this ran inside the rescan
    /// loop, and the previous `/bin/ps` implementation was a synchronous
    /// process spawn per legacy-manifest session per rescan on the main
    /// actor. Only works for same-user processes; a failure returns nil,
    /// which callers already treat as "cannot prove" (.unknown).
    private nonisolated static func processCommandLine(_ pid: Int32) -> String? {
        var mib: [Int32] = [CTL_KERN, KERN_PROCARGS2, pid]
        var size = 0
        guard sysctl(&mib, 3, nil, &size, nil, 0) == 0,
              size > MemoryLayout<Int32>.size else { return nil }
        var buffer = [UInt8](repeating: 0, count: size)
        guard sysctl(&mib, 3, &buffer, &size, nil, 0) == 0,
              size > MemoryLayout<Int32>.size else { return nil }
        // Layout: argc (Int32), exec path (NUL-terminated), NUL padding,
        // then argc NUL-terminated argv strings, then the environment.
        let argc = buffer.withUnsafeBytes { $0.load(as: Int32.self) }
        guard argc > 0 else { return nil }
        var index = MemoryLayout<Int32>.size
        while index < size, buffer[index] != 0 { index += 1 }
        while index < size, buffer[index] == 0 { index += 1 }
        var args: [String] = []
        var current: [UInt8] = []
        while index < size, args.count < Int(argc) {
            if buffer[index] == 0 {
                args.append(String(decoding: current, as: UTF8.self))
                current.removeAll(keepingCapacity: true)
            } else {
                current.append(buffer[index])
            }
            index += 1
        }
        let command = args.joined(separator: " ")
        return command.isEmpty ? nil : command
    }

    /// Freeze a settled title across the host teardown. Hosts from builds
    /// before 2026-07-21 rebuilt their final exit manifest from the
    /// launch-time session record, reverting the auto-title (or a
    /// manifest-level custom title) to the preset label the moment the host
    /// drained. New hosts preserve the on-disk record — but a live session
    /// keeps whatever host binary spawned it across app updates, so every
    /// user has a window of old-host sessions after updating. Snapshotting
    /// the title into the rename overlay (which always wins over the
    /// manifest label, and already carries across Restart) makes the title
    /// stick no matter which host generation writes last.
    private func preserveSettledTitleBeforeStop(_ session: SessionEntry) {
        var titles = loadSessionTitleOverrides()
        guard titles[session.id] == nil else { return }
        let label = session.label.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !label.isEmpty else { return }
        if session.command.isEmpty {
            // The folder label is still provisional. The Host flips
            // custom_title after the first submitted shell command; explicit
            // title markers are folded into SessionEntry.customTitle above.
            guard session.customTitle else { return }
        } else if session.command.hasPrefix(label) {
            // The label is still the display command — untitled, nothing
            // worth freezing.
            return
        }
        titles[session.id] = label
        saveSessionTitleOverrides(titles)
    }

    /// User-visible stop: kill the hosted PTY but keep the session directory
    /// and output history, so it remains in the sidebar as restartable.
    func stopSession(_ sessionID: String) -> Bool {
        guard remoteHostRuntime.supportsHostOperation(
            RemoteHostRuntime.HostOperation.stop
        ), remoteSummary(for: sessionID)?.status == .running
        else { return false }
        performRemoteVerb("Couldn't stop the session") { runtime in
            try await runtime.stopSession(sessionID)
        }
        return true
    }

    /// Copy the session's conversation transcript to the clipboard as Markdown.
    /// Rendered by `unpeel-host __transcript__ markdown <id>`, which reads the
    /// shared Settings ▸ Transcripts options from `app-state.json` — so the
    /// clipboard content and the Sessions MCP `read_transcript` output stay
    /// aligned. Runs off the main thread; the host resolves the provider
    /// transcript, so no path/args are needed here.
    /// `entries` overrides the Settings ▸ Transcripts range for this copy
    /// (the context menu's flyout picks): a count keeps that many most-recent
    /// entries, 0 means the whole conversation, nil uses the Settings default.
    func copyTranscriptMarkdown(_ sessionID: String, entries: Int? = nil) {
        if remoteSummary(for: sessionID) != nil {
            performRemoteVerb("Couldn't copy transcript") { runtime in
                let markdown = try await runtime.transcriptMarkdown(
                    sessionID: sessionID,
                    entries: entries
                )
                let trimmed = markdown.trimmingCharacters(in: .whitespacesAndNewlines)
                guard !trimmed.isEmpty else {
                    throw RemoteHostVerbError(
                        operation: "copy transcript",
                        message: "This session has no readable conversation transcript yet.",
                        outcomeIsUnknown: false
                    )
                }
                NSPasteboard.general.clearContents()
                NSPasteboard.general.setString(trimmed, forType: .string)
            }
            return
        }
        Task.detached {
            let outcome = Self.runTranscriptMarkdown(sessionID: sessionID, entries: entries)
            await MainActor.run {
                if let error = outcome.error {
                    Self.showErrorAlert(title: "Couldn't copy transcript", message: error)
                    return
                }
                let trimmed = outcome.markdown.trimmingCharacters(in: .whitespacesAndNewlines)
                guard !trimmed.isEmpty else {
                    Self.showErrorAlert(
                        title: "No transcript to copy",
                        message: "This session has no readable conversation transcript yet."
                    )
                    return
                }
                NSPasteboard.general.clearContents()
                NSPasteboard.general.setString(trimmed, forType: .string)
            }
        }
    }

    /// Runs the Host CLI off the main actor.
    /// `entries` maps to the CLI's `--entries` override (0 = whole
    /// conversation, matching TranscriptSettings.maxEntries semantics).
    nonisolated static func runTranscriptMarkdown(
        sessionID: String,
        entries: Int? = nil
    ) -> (markdown: String, error: String?) {
        let process = Process()
        process.executableURL = URL(fileURLWithPath: LaunchConfig.hostBinary)
        var arguments = ["__transcript__", "markdown", sessionID]
        if let entries {
            arguments += ["--entries", String(max(0, entries))]
        }
        process.arguments = arguments
        process.standardInput = FileHandle.nullDevice
        let stdout = Pipe()
        let stderr = Pipe()
        process.standardOutput = stdout
        process.standardError = stderr
        do {
            try process.run()
        } catch {
            return ("", "Failed to run unpeel-host: \(error.localizedDescription)")
        }
        let outData = stdout.fileHandleForReading.readDataToEndOfFile()
        let errData = stderr.fileHandleForReading.readDataToEndOfFile()
        process.waitUntilExit()
        if process.terminationStatus != 0 {
            let message = String(data: errData, encoding: .utf8)?
                .trimmingCharacters(in: .whitespacesAndNewlines)
            if let message, !message.isEmpty {
                return ("", message)
            }
            return ("", "unpeel-host exited with code \(process.terminationStatus).")
        }
        return (String(data: outData, encoding: .utf8) ?? "", nil)
    }

    /// End a session's cua-driver session (overlay cursor + scope state) via
    /// `unpeel-host __computer_cleanup__ <id>` (computer_mcp.rs run_cleanup).
    /// Piggybacks on every browser-cleanup site — both are "session is going
    /// away, reap its engine state" — and tolerates a stopped daemon.
    private nonisolated static func cleanupComputerSession(
        sessionID: String,
        unpeelHome: URL? = nil
    ) {
        let process = Process()
        process.executableURL = URL(fileURLWithPath: LaunchConfig.hostBinary)
        process.arguments = ["__computer_cleanup__", sessionID]
        process.standardInput = FileHandle.nullDevice
        process.standardOutput = FileHandle.nullDevice
        process.standardError = FileHandle.nullDevice
        if let unpeelHome {
            var env = ProcessInfo.processInfo.environment
            env["UNPEEL_HOME"] = unpeelHome.path
            process.environment = env
        }
        do {
            try process.run()
        } catch {
            NSLog("[UnpeelNative] failed to spawn computer cleanup: \(error)")
        }
    }

    /// One-shot Unix-socket request to the session host's control socket
    /// (newline-framed JSON, send_command_for_response in session_host.rs).
    private nonisolated static func sendSocketCommand(
        socketPath: String, payload: String
    ) -> Bool {
        let fd = socket(AF_UNIX, SOCK_STREAM, 0)
        guard fd >= 0 else { return false }
        defer { close(fd) }

        var addr = sockaddr_un()
        addr.sun_family = sa_family_t(AF_UNIX)
        let maxLen = MemoryLayout.size(ofValue: addr.sun_path) - 1
        let pathBytes = Array(socketPath.utf8)
        guard pathBytes.count <= maxLen else { return false }
        withUnsafeMutableBytes(of: &addr.sun_path) { dst in
            dst.copyBytes(from: pathBytes)
        }

        var tv = timeval(tv_sec: 1, tv_usec: 0)
        setsockopt(fd, SOL_SOCKET, SO_SNDTIMEO, &tv, socklen_t(MemoryLayout<timeval>.size))
        setsockopt(fd, SOL_SOCKET, SO_RCVTIMEO, &tv, socklen_t(MemoryLayout<timeval>.size))

        let connected = withUnsafePointer(to: &addr) { ptr in
            ptr.withMemoryRebound(to: sockaddr.self, capacity: 1) {
                connect(fd, $0, socklen_t(MemoryLayout<sockaddr_un>.size))
            }
        }
        guard connected == 0 else { return false }

        let sent = payload.withCString { send(fd, $0, strlen($0), 0) }
        guard sent > 0 else { return false }
        // Best-effort ack read so the host finishes handling before we
        // start polling the manifest.
        var buffer = [UInt8](repeating: 0, count: 256)
        _ = recv(fd, &buffer, buffer.count, 0)
        return true
    }

    // MARK: - Restart session (restartSession, stores/sessions.ts:554-590)

    /// Sessions whose restart is in flight (App.svelte restartingSessions):
    /// their rows/buttons render disabled until the replacement appears.
    @Published private(set) var restartingSessionIDs: Set<String> = [] {
        // A restarting (resuming) block counts as ACTIVE in the sidebar
        // partition, so the row jumps to its active-group spot the moment
        // Resume is clicked — the cache must see the flag flip.
        didSet { if restartingSessionIDs != oldValue { invalidateSidebarLists() } }
    }

    /// In-place managed-agent resumes. Unlike `restartingSessionIDs`, these
    /// never unmount the terminal: the Session id, PTY, attach surface, and
    /// scrollback all remain live while the ended provider is resumed.
    @Published private(set) var resumingAgentSessionIDs: Set<String> = [] {
        didSet { if resumingAgentSessionIDs != oldValue { invalidateSidebarLists() } }
    }

    /// Snapshot of each restarting session, kept so rebuildTree can hold its
    /// row in place across the window where the old host's manifest is gone
    /// and the replacement hasn't been spawned yet. Without it the row blinks
    /// out of the sidebar for the duration of kill+cleanup.
    private var restartPlaceholders: [String: SessionEntry] = [:]

    /// Host-routed Resume (`session.restart` on the selected Host). The Host
    /// tears the source Session down and publishes the replacement under a
    /// new id, and its snapshots can show the gap in between — the row would
    /// vanish and reappear. Hold the source row instead, lifted straight to
    /// the slot the replacement will take, until the replacement is
    /// published (resolved with the same correlation the runtime uses for
    /// selection) or the intent expires. In-memory only.
    private struct RemoteRestartPlaceholder {
        let summary: RemoteSessionSummary
        let entry: SessionEntry
        let intent: RemoteHostRuntime.ReplacementSelectionIntent
        /// The Host's shared rank list for the project when this Controller
        /// can read it (Local scope shares the Host's on-disk order file);
        /// empty otherwise, which treats every row as unranked.
        let sharedOrder: [String]
        let startedAt: Date
    }
    private var remoteRestartPlaceholders: [String: RemoteRestartPlaceholder] = [:]
    private static let remoteRestartPlaceholderTimeout: TimeInterval = 30

    /// Start holding `sessionID`'s row across a Host-routed restart.
    private func beginRemoteRestartPlaceholder(_ sessionID: String) {
        guard let summary = remoteSummary(for: sessionID) else { return }
        let entry = remoteSessionsByID[sessionID] ?? Self.sessionEntry(fromRemote: summary)
        let known = Set(remoteSummariesByID.keys)
            .union(remoteArchivedSummaryCache.sessionIDs)
        var sharedOrder: [String] = []
        if case .local = selectedHostScope {
            sharedOrder = Self.sharedSessionOrder(projectID: summary.projectID) ?? []
        }
        remoteRestartPlaceholders[sessionID] = RemoteRestartPlaceholder(
            summary: summary,
            entry: entry,
            intent: RemoteHostRuntime.ReplacementSelectionIntent(
                source: summary,
                knownSessionIDs: known
            ),
            sharedOrder: sharedOrder,
            startedAt: Date()
        )
        restartingSessionIDs.insert(sessionID)
        // Move the row to its landing slot now, not on the next Host poll.
        projectRemoteScope(snapshot: remoteHostRuntime.snapshot)
    }

    private func endRemoteRestartPlaceholder(_ sessionID: String) {
        guard remoteRestartPlaceholders.removeValue(forKey: sessionID) != nil else { return }
        restartingSessionIDs.remove(sessionID)
    }

    /// Index at which a Host-routed Resume's row should sit in a project's
    /// Host row order (`order`, with the source already removed) so it lands
    /// where the replacement will: the Host keeps created_at and the shared
    /// rank across a restart, and sorts running rows unranked-newest-first
    /// followed by ranked rows in rank order. With no running rows the row
    /// leads the stopped rows. Pure so the rule is testable.
    nonisolated static func predictedResumeInsertionIndex(
        in order: [String],
        sourceID: String,
        sourceCreatedAt: Int64,
        sharedOrder: [String],
        isRunningRow: (String) -> Bool,
        isSessionRow: (String) -> Bool,
        createdAt: (String) -> Int64
    ) -> Int {
        var rank: [String: Int] = [:]
        for (index, id) in sharedOrder.enumerated() { rank[id] = index }
        let sourceRank = rank[sourceID]
        var lastRunning: Int?
        for (index, id) in order.enumerated() where isRunningRow(id) {
            lastRunning = index
            let sortsAfterSource: Bool
            switch (sourceRank, rank[id]) {
            case let (source?, row?):
                sortsAfterSource = row > source
            case (.some, nil):
                // Unranked rows precede every ranked row.
                sortsAfterSource = false
            case (nil, .some):
                sortsAfterSource = true
            case (nil, nil):
                sortsAfterSource = createdAt(id) < sourceCreatedAt
            }
            if sortsAfterSource { return index }
        }
        if let lastRunning { return lastRunning + 1 }
        return order.firstIndex(where: isSessionRow) ?? order.count
    }

    /// Resume only the stable managed agent inside a live terminal. The Host
    /// re-derives the command and verifies the foreground owner; Swift never
    /// promotes a passively observed runtime or supplies relaunch argv.
    @discardableResult
    func resumeAgent(_ sessionID: String) -> Bool {
        guard sessionCanResumeAgent(sessionID),
              !resumingAgentSessionIDs.contains(sessionID),
              !restartingSessionIDs.contains(sessionID),
              !removingSessionIDs.contains(sessionID)
        else { return false }

        resumingAgentSessionIDs.insert(sessionID)

        performRemoteVerb(
            "Couldn't resume the agent",
            onFailure: { [weak self] in
                self?.resumingAgentSessionIDs.remove(sessionID)
            }
        ) { [weak self] runtime in
            try await runtime.resumeAgent(sessionID)
            self?.resumingAgentSessionIDs.remove(sessionID)
        }
        return true
    }

    /// The visible primary action: live managed launches restart only their
    /// agent; stopped Sessions retain the legacy replacement-based Resume.
    @discardableResult
    func resumeAgentOrSession(_ sessionID: String) -> Bool {
        guard let session = displaySessionsByID[sessionID] else { return false }
        if sessionIsRecentArchived(sessionID) {
            return resumeArchivedSession(sessionID)
        }
        return session.isLive ? resumeAgent(sessionID) : restartSession(sessionID)
    }

    nonisolated static func runResumeAgentHostCommand(
        sessionID: String
    ) -> ResumeAgentHostCommandFailure? {
        let process = Process()
        process.executableURL = URL(fileURLWithPath: LaunchConfig.hostBinary)
        process.arguments = ["__resume_agent__", sessionID]
        process.standardInput = FileHandle.nullDevice
        process.standardOutput = FileHandle.nullDevice
        let stderr = Pipe()
        process.standardError = stderr
        do {
            try process.run()
        } catch {
            return ResumeAgentHostCommandFailure(
                status: 500,
                message: error.localizedDescription
            )
        }
        process.waitUntilExit()
        guard process.terminationStatus != 0 else { return nil }
        let data = (try? stderr.fileHandleForReading.readToEnd()) ?? Data()
        let rawMessage = String(decoding: data, as: UTF8.self)
            .trimmingCharacters(in: .whitespacesAndNewlines)
        let message = normalizedResumeAgentFailureMessage(rawMessage)
        return ResumeAgentHostCommandFailure(
            status: resumeAgentFailureHTTPStatus(message),
            message: message.isEmpty
                ? "The terminal Host rejected the agent resume."
                : message
        )
    }

    /// Await the synchronous Host CLI receipt without occupying MainActor.
    /// The injectable runner keeps the executor boundary deterministic under
    /// test while production always invokes the real `unpeel-host` command.
    nonisolated static func runResumeAgentHostCommandOffMainActor(
        sessionID: String,
        runner: @escaping @Sendable (String) -> ResumeAgentHostCommandFailure? = {
            runResumeAgentHostCommand(sessionID: $0)
        }
    ) async -> ResumeAgentHostCommandFailure? {
        await Task.detached(priority: .userInitiated) {
            runner(sessionID)
        }.value
    }

    /// Keep native compatibility routing aligned with Rust
    /// `classify_restart_agent_failure`: only stable eligibility/concurrency
    /// races are 409. Transport, signal, support-install, PTY-write, timeout,
    /// and post-submit manifest failures are infrastructure/ambiguous 500s.
    nonisolated static func resumeAgentFailureHTTPStatus(_ message: String) -> Int {
        let message = normalizedResumeAgentFailureMessage(message)
        if message.hasPrefix("session "), message.hasSuffix(" is not running") {
            return 409
        }
        let exactConflicts = [
            "Agent restart requires a nonblank, known resumable launch command",
            "An agent restart is already in progress",
            "Session host no longer has an owned shell process",
            "Session host process identity could not be verified",
            "Session host has no verifiable process start time",
            "Session terminal has no foreground process group",
            "Terminal foreground is outside the owned session",
            "Agent foreground changed before restart escalation",
            "Agent stopped, but the owned shell did not regain the terminal",
            "Owned shell changed while restarting the agent",
            "Owned shell lost the terminal before agent relaunch",
        ]
        if message.hasPrefix("Agent restart generation changed")
            || message.hasPrefix("Refusing to restart ")
            || message.hasPrefix("Refusing to resume ")
            || (message.hasPrefix("session ")
                && message.hasSuffix(" host does not support shell-only agent resume"))
            || exactConflicts.contains(message) {
            return 409
        }
        return 500
    }

    /// `unpeel-host` prefixes command errors for CLI readability. Strip only
    /// that stable wrapper before classifying or returning the receipt body.
    nonisolated static func normalizedResumeAgentFailureMessage(_ message: String) -> String {
        let trimmed = message.trimmingCharacters(in: .whitespacesAndNewlines)
        for prefix in ["agent resume failed: ", "agent restart failed: "]
        where trimmed.hasPrefix(prefix) {
            return String(trimmed.dropFirst(prefix.count))
                .trimmingCharacters(in: .whitespacesAndNewlines)
        }
        return trimmed
    }

    nonisolated static func hookReceiptPredatesRuntimeLaunch(
        receivedAt: Date,
        launchedAt: Date?
    ) -> Bool {
        launchedAt.map { receivedAt < $0 } ?? false
    }

    /// Bind an owned hook to the Host's managed-runtime generation before it
    /// can mutate any native state. Exact generation provenance wins. Legacy
    /// hooks remain compatible for the initial launch. Immediately after an
    /// in-place edge a Stop cannot prove which process emitted it: a
    /// current-turn Start/UserPromptSubmit received after the launch boundary
    /// establishes ownership. The 30-second fallback bounds degradation for a
    /// permanently old hook install that emits no recognizable opener.
    nonisolated static func hookRuntimeDecision(
        eventGeneration: UInt64?,
        hookEventName: String,
        receivedAt: Date,
        currentGeneration: UInt64?,
        runtimeLaunchedAt: Date?,
        currentGenerationOwned: Bool
    ) -> HookRuntimeDecision {
        if let eventGeneration {
            if let currentGeneration, eventGeneration < currentGeneration {
                return .reject
            }
            // A hook can beat the manifest rescan (and, briefly, the manifest
            // commit visible to this process). Retain its exact future
            // generation; resetForRuntimeLaunch will bind it when the edge is
            // observed.
            return .accept(effectiveGeneration: eventGeneration)
        }

        guard let currentGeneration, currentGeneration > 1 else {
            return .accept(effectiveGeneration: nil)
        }
        if hookReceiptPredatesRuntimeLaunch(
            receivedAt: receivedAt,
            launchedAt: runtimeLaunchedAt
        ) {
            return .reject
        }

        switch hookEventName {
        case "Start", "UserPromptSubmit":
            return .accept(effectiveGeneration: currentGeneration)
        case "Stop", "StopFailure":
            if currentGenerationOwned {
                return .accept(effectiveGeneration: currentGeneration)
            }
            if let runtimeLaunchedAt,
               receivedAt.timeIntervalSince(runtimeLaunchedAt)
                   < Self.legacyGenerationStopGuard {
                return .reject
            }
            return .accept(effectiveGeneration: currentGeneration)
        default:
            // Non-completion events cannot create legacy Stop ownership. They
            // may still drive their own current activity transition after the
            // launch boundary (for example PermissionRequest).
            return .accept(effectiveGeneration: currentGeneration)
        }
    }

    /// Svelte restart semantics (restartSession + handleRestartSession,
    /// App.svelte:1213-1261): the old session is fully removed
    /// (kill_session for live hosts / close_saved_session for dead ones —
    /// both drop the entry from UI and persisted state), then a FRESH
    /// session (new id) is spawned with the session's original command and
    /// label. custom_title carries over; worktree sessions restart inside
    /// their worktree, not the project root; a pinned session is re-pinned;
    /// and created_at is stabilized to the old value so the row keeps its
    /// sidebar position. The provider conversation IS resumed where the CLI
    /// supports it, but only after the hosted PTY has received input. A
    /// never-written session has no provider conversation to resume, so it is
    /// relaunched with the original command. `forceFresh` strips every resume
    /// marker regardless — the recovery path when the provider conversation
    /// is gone from disk (see `resumeFailures`).
    @discardableResult
    func restartSession(
        _ sessionID: String,
        forceFresh: Bool = false,
        stoppedOnly: Bool = true
    ) -> Bool {
        guard !restartingSessionIDs.contains(sessionID) else { return false }
        beginRemoteRestartPlaceholder(sessionID)
        performRemoteVerb("Couldn't restart the session", onFailure: { [weak self] in
            self?.endRemoteRestartPlaceholder(sessionID)
        }) { runtime in
            try await runtime.restartSession(sessionID)
        }
        return true
    }

    // MARK: - Resume failure detection (ResumeFailedBar)

    /// Watch a freshly-launched runtime's earliest output for the runtime
    /// adapter's Host-published "conversation not found" markers.
    /// Replacement Sessions begin at byte zero; in-place Resume Agent uses
    /// the Host-committed generation boundary so old scrollback cannot match.
    private func watchForResumeFailure(
        sessionID: String,
        markers: [String],
        startOffset: UInt64 = 0
    ) {
        resumeFailureWatchers[sessionID]?.cancel()
        resumeFailureWatchers.removeValue(forKey: sessionID)
        resumeFailureWatcherTokens.removeValue(forKey: sessionID)
        resumeFailures.remove(sessionID)
        guard !markers.isEmpty else { return }
        let outputURL = LaunchConfig.appSessionsDir
            .appendingPathComponent(sessionID)
            .appendingPathComponent("output.bin")
        let token = UUID()
        resumeFailureWatcherTokens[sessionID] = token
        resumeFailureWatchers[sessionID] = Task { [weak self] in
            defer {
                Task { @MainActor [weak self] in
                    guard self?.resumeFailureWatcherTokens[sessionID] == token else {
                        return
                    }
                    self?.resumeFailureWatchers.removeValue(forKey: sessionID)
                    self?.resumeFailureWatcherTokens.removeValue(forKey: sessionID)
                }
            }
            // The error lands within a second or two of the CLI starting;
            // poll briefly and give up quietly (a successful resume, a slow
            // machine past the window, or a removed session all just end the
            // watch with no flag).
            for _ in 0..<15 {
                try? await Task.sleep(nanoseconds: 2_000_000_000)
                if Task.isCancelled { return }
                guard let launchOutput = Self.readFileWindow(
                    outputURL, fromOffset: startOffset, maxBytes: 8192
                ) else {
                    continue
                }
                let text = String(decoding: launchOutput, as: UTF8.self)
                guard markers.allSatisfy(text.contains) else { continue }
                await MainActor.run { [weak self] in
                    guard let self,
                          self.resumeFailureWatcherTokens[sessionID] == token,
                          self.sessionsByID[sessionID] != nil
                    else { return }
                    self.resumeFailures.insert(sessionID)
                }
                return
            }
        }
    }

    func dismissResumeFailure(for sessionID: String) {
        resumeFailures.remove(sessionID)
    }

    /// ResumeFailedBar's action: relaunch without any resume marker. The dead
    /// conversation is unrecoverable (its provider storage is gone), so a
    /// fresh start — with a newly minted conversation id where the provider
    /// supports one — is the only forward path. The flag is not cleared here:
    /// a successful restart prunes it with the old session id
    /// (pruneNativeState), and a refused restart should keep the bar up for
    /// another try.
    func startFreshAfterResumeFailure(_ sessionID: String) {
        restartSession(sessionID, forceFresh: true, stoppedOnly: false)
    }

    /// Up to `maxBytes` at a stable output-generation boundary, nil when the
    /// file is unreadable. Seeking past the current end yields empty data and
    /// lets the watcher retry as output arrives.
    nonisolated static func readFileWindow(
        _ url: URL,
        fromOffset offset: UInt64,
        maxBytes: Int
    ) -> Data? {
        guard let handle = try? FileHandle(forReadingFrom: url) else { return nil }
        defer { try? handle.close() }
        let logicalEnd = (try? handle.seekToEnd()) ?? 0
        let retentionURL = url.deletingLastPathComponent()
            .appendingPathComponent("output-retention.json")
        let retainedFrom: UInt64 = {
            guard let data = try? Data(contentsOf: retentionURL),
                  let object = try? JSONSerialization.jsonObject(with: data) as? [String: Any],
                  (object["version"] as? NSNumber)?.uint32Value == 1,
                  let retained = (object["retained_from"] as? NSNumber)?.uint64Value
            else { return 0 }
            return min(retained, logicalEnd)
        }()
        let readableOffset = max(offset, retainedFrom)
        do {
            try handle.seek(toOffset: readableOffset)
        } catch {
            return nil
        }
        return try? handle.read(upToCount: maxBytes)
    }

    /// Defense in depth for destructive cleanup. The Host already validates
    /// the runtime-owned path before publishing it; native independently
    /// resolves symlinks and accepts only a strict descendant of this
    /// instance's UNPEEL_HOME.
    nonisolated static func validatedManagedStoragePath(
        _ path: String?,
        unpeelDir: URL
    ) -> String? {
        guard let path, !path.isEmpty else { return nil }
        let root = unpeelDir.standardizedFileURL.resolvingSymlinksInPath()
        let candidate = URL(fileURLWithPath: path)
            .standardizedFileURL
            .resolvingSymlinksInPath()
        let rootPrefix = root.path.hasSuffix("/") ? root.path : root.path + "/"
        guard candidate.path.hasPrefix(rootPrefix) else { return nil }
        return candidate.path
    }

    // Per-verb gates for the session context menu (and, via
    // RemoteDTOAdapters, the phone's session sheet). All derive from
    // ProviderCapabilities — the one place that knows what each CLI supports.

    /// Resume is offered only for a stopped Session where relaunching
    /// continues a provider-owned conversation.
    /// Live managed launches that returned to their shell use the separate
    /// Resume Agent gate below.
    func sessionCanRestart(_ sessionID: String) -> Bool {
        if routesSessionVerbThroughHost(sessionID),
           let summary = remoteSummary(for: sessionID) {
            guard summary.status == .exited else { return false }
            return (summary.capabilities?.restart ?? false)
                && remoteHostRuntime.supportsHostOperation(
                    RemoteHostRuntime.HostOperation.restart
                )
        }
        guard let session = sessionsByID[sessionID] else { return false }
        guard session.status == .exited else { return false }
        return ProviderCapabilities.canArchive(session: session)
    }

    /// Archive remains available for any resumable launch regardless of live
    /// state. It is separate from stopped-only Resume and live Resume Agent.
    func sessionCanArchive(_ sessionID: String) -> Bool {
        if routesSessionVerbThroughHost(sessionID),
           let summary = remoteSummariesByID[sessionID] {
            return summary.capabilities?.archive == true
                && remoteHostRuntime.supportsHostOperation(
                    RemoteHostRuntime.HostOperation.archive
                )
        }
        guard let session = sessionsByID[sessionID] else { return false }
        return ProviderCapabilities.canArchive(session: session)
    }

    /// Whether the live Session can resume its stable managed launch inside
    /// the same terminal. Remote scope requires both the Host-level operation
    /// and the Host-computed per-Session capability; absence from an older
    /// bootstrap is unsupported, never a cue to fall back to destructive
    /// legacy Session restart.
    func sessionCanResumeAgent(_ sessionID: String) -> Bool {
        if routesSessionVerbThroughHost(sessionID),
           let summary = remoteSummariesByID[sessionID] {
            guard summary.status == .running,
                  summary.activity != .starting,
                  summary.activeRuntimeID == nil,
                  !summary.runtimeLaunchPending,
                  summary.capabilities?.resumeAgent == true
            else { return false }
            return remoteHostRuntime.supportsHostOperation(
                RemoteHostRuntime.HostOperation.resumeAgent
            )
        }
        guard let session = sessionsByID[sessionID], session.status != .starting else {
            return false
        }
        return session.hasResumableState && ProviderCapabilities.canResumeAgent(
            command: session.command,
            isLive: session.isLive,
            activeRuntimeID: session.activeRuntimeID,
            runtimeLaunchPending: session.runtimeLaunchPending,
            hostProtocolVersion: session.hostProtocolVersion
        )
    }

    /// "Notify when done" needs a reliable hook Stop signal; recognized
    /// hookless agents such as Pi don't get the toggle. Neither do ordinary
    /// terminals, which have no agent-activity lifecycle at all.
    func sessionCanNotifyWhenDone(_ sessionID: String) -> Bool {
        if routesSessionVerbThroughHost(sessionID) {
            guard remoteHostRuntime.supportsHostOperation(
                RemoteHostRuntime.HostOperation.notifyWhenDoneSet
            ) else { return false }
            // The Local disk catalog cannot infer runtime capabilities from a
            // platform adapter, but the app already has the exact Session.
            if selectedHostScope == .local, let session = sessionsByID[sessionID] {
                return ProviderCapabilities.canNotifyWhenDone(session: session)
            }
            return remoteSummary(for: sessionID)?.capabilities?.notifyWhenDone == true
        }
        guard let session = sessionsByID[sessionID] else { return false }
        return ProviderCapabilities.canNotifyWhenDone(session: session)
    }

    /// "Clear attention" is a Controller-local activity-engine escape hatch;
    /// there is no remote operation for it. Pending MCP approvals are not
    /// dismissable this way — they need an explicit Allow / Don't Allow.
    func sessionCanClearAttention(_ sessionID: String) -> Bool {
        selectedHostScope == .local && !sessionNeedsMcpApprovalAttention(sessionID)
    }

    // MARK: - Project context menu actions (ProjectItem.svelte:818-941)

    /// Project id whose sidebar row shows the inline "Remove project?"
    /// confirm (native pattern — the Svelte app removes immediately for
    /// plain projects and uses a native dialog only for worktrees).
    @Published var confirmingRemoveProjectID: String? {
        didSet {
            if confirmingRemoveProjectID == nil {
                scheduleDeferredRemoteProjectionFlush()
            }
        }
    }
    private var projectWorkspaceMoveInFlight = false
    private var workspaceMoveTargetsCache:
        (stamp: Date?, source: String, defaultName: String, targets: [WorkspaceMoveTarget])?

    static let removedProjectsKey = "unpeel.native.removedProjects"

    /// Tombstoned project ids (native "Remove project"), pruned of ids that
    /// no longer exist in any source — a project re-added later (new id in
    /// Tauri) must not stay hidden, and the list must not grow unbounded.
    private func removedProjectIDs(prunedAgainst knownIDs: Set<String>) -> Set<String> {
        let stored = AppDefaults.shared.stringArray(forKey: Self.removedProjectsKey) ?? []
        let kept = stored.filter { knownIDs.contains($0) }
        if kept.count != stored.count {
            if kept.isEmpty {
                AppDefaults.shared.removeObject(forKey: Self.removedProjectsKey)
            } else {
                AppDefaults.shared.set(kept, forKey: Self.removedProjectsKey)
            }
        }
        return Set(kept)
    }

    func requestRemoveProject(_ projectID: String) {
        confirmingRemoveProjectID = projectID
    }

    func cancelRemoveProjectConfirm() {
        confirmingRemoveProjectID = nil
    }

    /// Remove a project from the sidebar. Parity with remove_project
    /// (project.rs:164-183): the project disappears from the tree along
    /// with its worktree children, its per-project UI state is dropped, and
    /// live hosted sessions are NOT killed (the Tauri app leaves hosts
    /// running too — it only forgets the project). Tauri-owned projects get
    /// a tombstone in `unpeel.native.removedProjects`; natively-added
    /// projects are actually deleted from `unpeel.native.projects`.
    func removeProject(_ projectID: String) {
        if confirmingRemoveProjectID == projectID {
            confirmingRemoveProjectID = nil
        }
        if selectedHostScope.scopedLocalHome != nil {
            removeScopedWorkspaceProject(projectID)
            return
        }
        if selectedHostScope == .local, localHostClientStarted {
            removeLocalHostProjectedProject(projectID)
            return
        }
        guard let project = projectsByID[projectID] else { return }

        // Natively-added project → drop the record itself.
        var records = loadNativeProjects()
        if records.contains(where: { $0.id == projectID }) {
            records.removeAll { $0.id == projectID }
            if records.isEmpty {
                AppDefaults.shared.removeObject(forKey: Self.nativeProjectsKey)
            } else if let data = try? JSONEncoder().encode(records) {
                AppDefaults.shared.set(data, forKey: Self.nativeProjectsKey)
            }
            mirrorProjectsToSharedState()
        } else {
            // Tauri-owned (or ephemeral) → tombstone overlay, and delete the
            // entry from the shared file: the tombstone only hides it from
            // THIS app, and a project the user removed must disappear from
            // every frontend, not linger in the terminal's sidebar.
            var removed = AppDefaults.shared.stringArray(forKey: Self.removedProjectsKey) ?? []
            if !removed.contains(projectID) {
                removed.append(projectID)
                AppDefaults.shared.set(removed, forKey: Self.removedProjectsKey)
            }
            ephemeralProjects.removeAll { $0.id == projectID }
            editPresetStateAnnouncing { object in
                var projects = (object["projects"] as? [[String: Any]]) ?? []
                let before = projects.count
                projects.removeAll { ($0["id"] as? String) == projectID }
                if projects.count != before {
                    object["projects"] = projects
                }
            }
        }

        // Per-project native state: expansion, reveal, order overlay, and
        // the selection if it lived in this project (or one of its worktree
        // children).
        let removedIDs = projectIDAndWorktreeDescendants(projectID)
        var removedFolderColor = false
        for id in removedIDs {
            expandedProjectIDs.remove(id)
            if projectFolderColorIDs.removeValue(forKey: id) != nil {
                removedFolderColor = true
            }
            AppDefaults.shared.removeObject(forKey: Self.sessionOrderKey(id))
            AppDefaults.shared.removeObject(forKey: Self.pinnedOrderKey(id))
            if archivedProjectID == id { archivedProjectID = nil }
            if let selected = selectedSessionID,
               sessionsByID[selected]?.projectID == id {
                selectedSessionID = nil
            }
        }
        if removedFolderColor {
            saveProjectFolderColorIDs()
        }
        // Project-order overlay slots: top-level order, per-parent worktree
        // orders, and any list owned by a removed project.
        pruneProjectOrderOverlays(removing: removedIDs)

        _ = project
        // Remove is the destructive verb: the subtree's sessions (project
        // root, worktree checkouts, plain groups) go with the project.
        // Leaving their dirs on disk made them unreachable here — the tree
        // only renders buckets for known projects — while the terminal UI
        // resurrected them as phantom cwd-named projects (2026-08-13).
        var doomedProjectIDs = removedIDs
        var stack = Array(removedIDs)
        while let current = stack.popLast() {
            for child in projectsByID.values
            where child.parentProjectID == current && !doomedProjectIDs.contains(child.id) {
                doomedProjectIDs.insert(child.id)
                stack.append(child.id)
            }
        }
        // Same bucket keying as rebuildTree: a valid override target wins
        // over the manifest project, so a session filed elsewhere by the
        // user survives its launch project's removal (and vice versa).
        let knownProjectIDs = Set(projectsByID.keys)
        for session in sessionsByID.values {
            let bucket = session.projectOverrideID.flatMap {
                knownProjectIDs.contains($0) ? $0 : nil
            } ?? session.projectID
            if doomedProjectIDs.contains(bucket) {
                confirmRemoveSession(session.id)
            }
        }
        rescan()
    }

    /// Local client-only Remove. The projected Host model can contain a legacy
    /// project that the compatibility Swift model already hid with a tombstone,
    /// so it must never look the id up in `projectsByID`. Project records remain
    /// a sanctioned local-against-home write until the Host protocol gains an
    /// Add/Remove Project verb; Session teardown still rides the Host contract.
    private func removeLocalHostProjectedProject(_ projectID: String) {
        guard remoteProjectsByID[projectID] != nil else { return }
        let parentByProjectID = remoteProjectsByID.values.reduce(
            into: [String: String]()
        ) { result, project in
            if let parentID = project.parentProjectID {
                result[project.id] = parentID
            }
        }
        let removedIDs = Self.projectSubtreeIDs(
            roots: [projectID],
            parentByProjectID: parentByProjectID
        )
        let doomedSessionIDs = remoteSessionsByID.values.compactMap { session in
            removedIDs.contains(session.projectID) ? session.id : nil
        }.sorted()

        let priorTombstones = AppDefaults.shared.stringArray(
            forKey: Self.removedProjectsKey
        ) ?? []
        if !priorTombstones.contains(projectID) {
            AppDefaults.shared.set(
                priorTombstones + [projectID],
                forKey: Self.removedProjectsKey
            )
        }
        let wrote = editPresetStateAnnouncing { object in
            var projects = (object["projects"] as? [[String: Any]]) ?? []
            projects.removeAll { entry in
                (entry["id"] as? String).map(removedIDs.contains) ?? false
            }
            object["projects"] = projects
        }
        guard wrote else {
            if priorTombstones.isEmpty {
                AppDefaults.shared.removeObject(forKey: Self.removedProjectsKey)
            } else {
                AppDefaults.shared.set(priorTombstones, forKey: Self.removedProjectsKey)
            }
            Self.showErrorAlert(
                title: "Couldn't remove the project",
                message: "Unpeel couldn't update this workspace's shared project state."
            )
            return
        }

        var records = loadNativeProjects()
        let priorRecordCount = records.count
        records.removeAll { removedIDs.contains($0.id) }
        if records.count != priorRecordCount {
            if records.isEmpty {
                AppDefaults.shared.removeObject(forKey: Self.nativeProjectsKey)
            } else if let data = try? JSONEncoder().encode(records) {
                AppDefaults.shared.set(data, forKey: Self.nativeProjectsKey)
            }
        }

        for id in removedIDs {
            expandedProjectIDs.remove(id)
            projectFolderColorIDs.removeValue(forKey: id)
            AppDefaults.shared.removeObject(forKey: Self.sessionOrderKey(id))
            AppDefaults.shared.removeObject(forKey: Self.pinnedOrderKey(id))
            if archivedProjectID == id { archivedProjectID = nil }
        }
        saveProjectFolderColorIDs()
        pruneProjectOrderOverlays(removing: removedIDs)
        if let selectedSessionID,
           doomedSessionIDs.contains(selectedSessionID) {
            self.selectedSessionID = nil
        }

        remoteHostRuntime.requestImmediateRefresh()
        if !doomedSessionIDs.isEmpty {
            performRemoteVerb("Couldn't remove the project's sessions") { runtime in
                for sessionID in doomedSessionIDs {
                    try await runtime.removeSession(sessionID)
                }
            }
        }
    }

    /// `.localWorkspace` Remove: forget the project from the SCOPED
    /// workspace's own home (its suite record if natively-added, plus its
    /// `app-state.json` projects list — a tombstone for a file-owned entry),
    /// then nudge the gateway. Sessions are left running exactly like the
    /// local Remove: this only forgets the project. Destructive session
    /// teardown for a scoped workspace is deferred (no gateway remove-project
    /// verb yet); the sidebar simply stops rendering the forgotten project.
    private func removeScopedWorkspaceProject(_ projectID: String) {
        let defaults = scopedAppDefaults
        var records = Self.loadNativeProjects(from: defaults)
        let wasNative = records.contains { $0.id == projectID }
        if wasNative {
            records.removeAll { $0.id == projectID }
            if records.isEmpty {
                defaults.removeObject(forKey: Self.nativeProjectsKey)
            } else if let data = try? JSONEncoder().encode(records) {
                defaults.set(data, forKey: Self.nativeProjectsKey)
            }
        }
        editScopedAppStateAnnouncing { object in
            var projects = (object["projects"] as? [[String: Any]]) ?? []
            let before = projects.count
            projects.removeAll { ($0["id"] as? String) == projectID }
            if projects.count != before {
                object["projects"] = projects
            }
        }
        if archivedProjectID == projectID { archivedProjectID = nil }
        if let selected = selectedSessionID,
           displaySessionsByID[selected]?.projectID == projectID {
            selectedSessionID = nil
        }
    }

    /// Other local workspaces a top-level project can be filed into. Empty
    /// when this Mac only has one workspace, or when the current scope is a
    /// true remote Host.
    func localWorkspaceMoveTargets() -> [WorkspaceMoveTarget] {
        guard selectedHostScope.isLocalMachine,
              let sourceHome = scopedLocalUnpeelDir
        else { return [] }
        let source = UnpeelWorkspaceRegistry.normalizePath(sourceHome.path)
        let stamp = (try? FileManager.default.attributesOfItem(
            atPath: UnpeelWorkspaceRegistry.registryURL.path
        ))?[.modificationDate] as? Date
        let defaultName = UnpeelWorkspaceContext.defaultWorkspaceName ?? "Personal"
        if let cache = workspaceMoveTargetsCache,
           cache.stamp == stamp,
           cache.source == source,
           cache.defaultName == defaultName
        {
            return cache.targets
        }
        let targets = WorkspaceListOrder.apply(
            to: Self.knownLocalWorkspaceHomesAndNames().compactMap { home, name
                -> WorkspaceMoveTarget? in
                let normalized = UnpeelWorkspaceRegistry.normalizePath(home)
                guard normalized != source else { return nil }
                return WorkspaceMoveTarget(id: normalized, name: name, home: home)
            },
            key: { WorkspaceListOrder.localKey(home: $0.id) }
        )
        workspaceMoveTargetsCache = (stamp, source, defaultName, targets)
        return targets
    }

    /// Move the project record and its session dirs into another local
    /// workspace on this Mac. Live hosts keep running — the session dir
    /// is renamed, not copied.
    func moveProjectToLocalWorkspace(projectID: String, dest: WorkspaceMoveTarget) {
        guard selectedHostScope.isLocalMachine,
              let sourceHome = scopedLocalUnpeelDir,
              !projectWorkspaceMoveInFlight
        else { return }
        let destHome = URL(fileURLWithPath: dest.home, isDirectory: true)
        guard UnpeelWorkspaceRegistry.normalizePath(sourceHome.path)
            != UnpeelWorkspaceRegistry.normalizePath(destHome.path)
        else { return }
        // Only top-level projects: a group or worktree child has nowhere to
        // hang in the destination without its parent.
        let project = displayProjectsByID[projectID] ?? projectsByID[projectID]
        guard let project, project.parentProjectID == nil else { return }

        projectWorkspaceMoveInFlight = true
        if selectedHostScope == .local {
            mirrorProjectsToSharedState()
        }

        Task { @MainActor [weak self] in
            guard let self else { return }
            defer { self.projectWorkspaceMoveInFlight = false }

            let outcome: ProjectWorkspaceMove.Outcome
            do {
                outcome = try ProjectWorkspaceMove.move(
                    projectID: projectID,
                    from: sourceHome,
                    to: destHome
                )
            } catch {
                Self.showErrorAlert(
                    title: "Couldn't move project",
                    message: error.localizedDescription
                )
                return
            }

            self.transferWorkspaceOverlays(
                projectIDs: outcome.projectIDs,
                sessionIDs: outcome.sessionIDs,
                sourceHome: sourceHome,
                destHome: destHome
            )
            self.forgetMovedProjectLocally(outcome)
            Self.announceStateChange(
                "app-state",
                registry: sourceHome.appendingPathComponent("app-ports"),
                ownPort: self.hookServer?.port
            )
            Self.announceStateChange(
                "app-state",
                registry: destHome.appendingPathComponent("app-ports"),
                ownPort: self.hookServer?.port
            )
            self.workspacePool.dropSnapshot(
                forKey: WorkspaceListOrder.localKey(home: dest.home)
            )
            self.workspacePool.requestImmediateRefresh()
            self.rescopeAfterProjectMove(to: dest)
            let switched = WorkspaceFeature.pickerEnabled
                || UnpeelWorkspaceRegistry.normalizePath(dest.home)
                    == Self.currentInstanceNormalizedHome()
            ToastCenter.shared.show(
                switched
                    ? "Moved \(outcome.rootProjectName) to \(dest.name)"
                    : "Moved \(outcome.rootProjectName) to \(dest.name). Open that workspace to see it.",
                systemImage: "arrow.right.square"
            )
        }
    }

    /// Native overlays live per-workspace in that home's defaults suite —
    /// app-state / session dirs are not enough for dest's own app instance
    /// (it will tombstone `native-` projects it does not own).
    private func transferWorkspaceOverlays(
        projectIDs: Set<String>,
        sessionIDs: [String],
        sourceHome: URL,
        destHome: URL
    ) {
        let sourceDefaults = Self.defaultsSuite(forWorkspaceHome: sourceHome.path)
        let destDefaults = Self.defaultsSuite(forWorkspaceHome: destHome.path)

        var sourceRecords = Self.loadNativeProjects(from: sourceDefaults)
        let movingRecords = sourceRecords.filter { projectIDs.contains($0.id) }
        if !movingRecords.isEmpty {
            sourceRecords.removeAll { projectIDs.contains($0.id) }
            Self.saveNativeProjects(sourceRecords, to: sourceDefaults)
            var destRecords = Self.loadNativeProjects(from: destDefaults)
            let destIDs = Set(destRecords.map(\.id))
            destRecords.append(contentsOf: movingRecords.filter { !destIDs.contains($0.id) })
            Self.saveNativeProjects(destRecords, to: destDefaults)
        }

        var sourceColors = Self.loadProjectFolderColorIDs(from: sourceDefaults)
        var destColors = Self.loadProjectFolderColorIDs(from: destDefaults)
        var movedColor = false
        for id in projectIDs {
            if let color = sourceColors.removeValue(forKey: id) {
                destColors[id] = color
                movedColor = true
            }
        }
        if movedColor {
            Self.saveProjectFolderColorIDs(sourceColors, to: sourceDefaults)
            Self.saveProjectFolderColorIDs(destColors, to: destDefaults)
        }

        var sourceTitles = (sourceDefaults.dictionary(
            forKey: NativeOverlay.sessionTitlesKey
        ) as? [String: String]) ?? [:]
        var destTitles = (destDefaults.dictionary(
            forKey: NativeOverlay.sessionTitlesKey
        ) as? [String: String]) ?? [:]
        var movedTitle = false
        for sessionID in sessionIDs {
            if let title = sourceTitles.removeValue(forKey: sessionID) {
                destTitles[sessionID] = title
                movedTitle = true
            }
        }
        if movedTitle {
            if sourceTitles.isEmpty {
                sourceDefaults.removeObject(forKey: NativeOverlay.sessionTitlesKey)
            } else {
                sourceDefaults.set(sourceTitles, forKey: NativeOverlay.sessionTitlesKey)
            }
            destDefaults.set(destTitles, forKey: NativeOverlay.sessionTitlesKey)
        }

        for id in projectIDs {
            let sessionKey = Self.sessionOrderKey(id)
            let pinnedKey = Self.pinnedOrderKey(id)
            if let order = sourceDefaults.stringArray(forKey: sessionKey) {
                destDefaults.set(order, forKey: sessionKey)
                sourceDefaults.removeObject(forKey: sessionKey)
            }
            if let pinned = sourceDefaults.stringArray(forKey: pinnedKey) {
                destDefaults.set(pinned, forKey: pinnedKey)
                sourceDefaults.removeObject(forKey: pinnedKey)
            }
        }

        var removed = destDefaults.stringArray(forKey: Self.removedProjectsKey) ?? []
        let before = removed.count
        removed.removeAll(where: projectIDs.contains)
        if removed.count != before {
            if removed.isEmpty {
                destDefaults.removeObject(forKey: Self.removedProjectsKey)
            } else {
                destDefaults.set(removed, forKey: Self.removedProjectsKey)
            }
        }
    }

    private func forgetMovedProjectLocally(_ outcome: ProjectWorkspaceMove.Outcome) {
        for id in outcome.projectIDs {
            expandedProjectIDs.remove(id)
            projectFolderColorIDs.removeValue(forKey: id)
            sessionOrderPreviews.removeValue(forKey: id)
            if archivedProjectID == id { archivedProjectID = nil }
        }
        saveProjectFolderColorIDs()
        if let selected = selectedSessionID,
           outcome.sessionIDs.contains(selected)
            || (sessionsByID[selected] ?? displaySessionsByID[selected])
                .map({ outcome.projectIDs.contains($0.projectID) }) == true
        {
            selectedSessionID = nil
        }
        if selectedHostScope == .local {
            rescan()
        } else {
            remoteHostRuntime.requestImmediateRefresh()
        }
    }

    private func rescopeAfterProjectMove(to dest: WorkspaceMoveTarget) {
        let destNorm = UnpeelWorkspaceRegistry.normalizePath(dest.home)
        if destNorm == Self.currentInstanceNormalizedHome() {
            if selectedHostScope != .local {
                selectHost(nil)
            }
            return
        }
        guard WorkspaceFeature.pickerEnabled else { return }
        if let row = WorkspaceSwitching.orderedRows(store: self).first(where: {
            $0.home.map { UnpeelWorkspaceRegistry.normalizePath($0) } == destNorm
        }) {
            WorkspaceSwitching.selectSliding(row, store: self)
        } else {
            selectLocalWorkspace(home: dest.home, name: dest.name)
        }
    }

    private static func defaultsSuite(forWorkspaceHome home: String) -> UserDefaults {
        let normalized = UnpeelWorkspaceRegistry.normalizePath(home)
        let defaultHome = UnpeelWorkspaceRegistry.normalizePath(
            UnpeelWorkspaceRegistry.realUnpeelDir.path
        )
        if normalized == defaultHome { return .standard }
        return AppDefaults.suite(forUnpeelHome: home)
    }

    private static func saveNativeProjects(
        _ records: [NativeProjectRecord],
        to defaults: UserDefaults
    ) {
        if records.isEmpty {
            defaults.removeObject(forKey: nativeProjectsKey)
        } else if let data = try? JSONEncoder().encode(records) {
            defaults.set(data, forKey: nativeProjectsKey)
        }
    }

    private static func loadProjectFolderColorIDs(
        from defaults: UserDefaults
    ) -> [String: String] {
        let raw = defaults.dictionary(forKey: nativeProjectFolderColorsKey) ?? [:]
        return raw.compactMapValues { value in
            guard let string = value as? String,
                  ProjectFolderColor(rawValue: string) != nil
            else { return nil }
            return string
        }
    }

    private static func saveProjectFolderColorIDs(
        _ colors: [String: String],
        to defaults: UserDefaults
    ) {
        if colors.isEmpty {
            defaults.removeObject(forKey: nativeProjectFolderColorsKey)
        } else {
            defaults.set(colors, forKey: nativeProjectFolderColorsKey)
        }
    }

    /// Reveal in Finder (project-menu-reveal → reveal_in_finder, which is
    /// `open -R` on the project path).
    func revealInFinder(path: String) {
        NSWorkspace.shared.activateFileViewerSelecting(
            [URL(fileURLWithPath: path)]
        )
    }

    /// Synchronous `.git` presence check standing in for the Tauri
    /// `is_git_repo` command (worktree.rs) that gates the worktree menu
    /// items. `.git` can be a dir (checkout) or a file (worktree/submodule).
    /// Cached with a short TTL: the sidebar calls this per project row per
    /// render pass, and a stat per row per publish adds up. A `git init`
    /// after launch still shows up within the TTL.
    nonisolated static func isGitRepo(path: String) -> Bool {
        let now = Date()
        return gitRepoCacheLock.withLock {
            if let cached = gitRepoCache[path],
               now.timeIntervalSince(cached.at) < 10 {
                return cached.isRepo
            }
            let isRepo = FileManager.default.fileExists(atPath: path + "/.git")
            gitRepoCache[path] = (now, isRepo)
            return isRepo
        }
    }

    private nonisolated(unsafe) static var gitRepoCache: [String: (at: Date, isRepo: Bool)] = [:]
    private nonisolated static let gitRepoCacheLock = NSLock()

    /// Stop all (project-menu-stop-all): stop every live session of THIS
    /// project — worktree children keep theirs, like the Svelte per-project
    /// session map. Stop is non-destructive: each host is killed through the
    /// identity-guarded `stopSession` path (the same verb the phone's session
    /// sheet uses), but the session dir AND its sidebar row survive — the row
    /// settles into the exited state, from where Restart resumes the
    /// conversation. (Until 2026-07-21 this reused the remove path and
    /// destroyed the sessions outright.)
    func stopAllSessions(projectID: String) {
        let live = remoteSessionsByID.values
            .filter { $0.projectID == projectID && $0.isLive }
            .map(\.id)
            .sorted()
        guard !live.isEmpty else { return }
        performRemoteVerb("Couldn't stop the sessions") { runtime in
            for sessionID in live {
                try await runtime.stopSession(sessionID)
            }
        }
    }

    // MARK: - Open in editor (open_in_editor, project.rs:577-615)

    /// Menu label for the configured editor id (editorLabel map,
    /// ProjectItem.svelte:824-831); unknown ids show the raw command.
    nonisolated static func editorDisplayName(_ editor: String) -> String {
        switch editor {
        case "code": return "VS Code"
        case "cursor": return "Cursor"
        case "zed": return "Zed"
        case "idea": return "IntelliJ"
        case "webstorm": return "WebStorm"
        case "xcode": return "Xcode"
        default: return editor
        }
    }

    /// Bundled CLI shims tried before anything else
    /// (preferred_editor_command_candidates, project.rs): VS Code's and
    /// Cursor's `code`/`cursor` CLIs live inside the app bundle, so they
    /// work even when the user never ran "install command in PATH".
    private nonisolated static func bundledEditorCLIs(_ editor: String) -> [String] {
        let home = NSHomeDirectory()
        switch editor {
        case "cursor":
            return [
                "/Applications/Cursor.app/Contents/Resources/app/bin/cursor",
                "\(home)/Applications/Cursor.app/Contents/Resources/app/bin/cursor",
            ]
        case "code":
            return [
                "/Applications/Visual Studio Code.app/Contents/Resources/app/bin/code",
                "\(home)/Applications/Visual Studio Code.app/Contents/Resources/app/bin/code",
            ]
        default:
            return []
        }
    }

    /// App-name fallback (fallback_editor_app_name, project.rs tests):
    /// `open -a <App>` when the CLI shim isn't found.
    private nonisolated static func fallbackEditorApp(_ editor: String) -> String? {
        switch editor {
        case "cursor": return "Cursor"
        case "code": return "Visual Studio Code"
        case "zed": return "Zed"
        case "idea": return "IntelliJ IDEA"
        case "webstorm": return "WebStorm"
        case "xcode": return "Xcode"
        default: return nil
        }
    }

    /// Same resolution order as project.rs open_in_editor: bundled CLI →
    /// `open -a <App>` → the editor id as a PATH command. Errors surface in
    /// an alert (the Svelte app shows a toast).
    func openInEditor(path: String) {
        Self.openInEditor(editor: codeEditor, path: path, line: nil, column: nil)
    }

    /// Opens a file from a cmd-click in the terminal at the given line/column,
    /// in the user's selected editor. Static so the terminal pane can call it
    /// without a store instance.
    nonisolated static func openFileInPreferredEditor(path: String, line: Int?, column: Int?) {
        openInEditor(editor: preferredCodeEditor(), path: path, line: line, column: column)
    }

    private nonisolated static func openInEditor(
        editor: String,
        path: String,
        line: Int?,
        column: Int?
    ) {
        Task {
            let error = await Task.detached(priority: .userInitiated) {
                openInEditorImpl(editor: editor, path: path, line: line, column: column)
            }.value
            if let error {
                await MainActor.run {
                    showErrorAlert(
                        title: "Couldn't open \(editorDisplayName(editor))",
                        message: error
                    )
                }
            }
        }
    }

    /// Per-editor arguments to open `path`, jumping to `line`/`column` when the
    /// editor's CLI supports it. Editors without a goto flag just get the path.
    private nonisolated static func editorOpenArguments(
        editor: String,
        path: String,
        line: Int?,
        column: Int?
    ) -> [String] {
        guard let line else { return [path] }
        switch editor {
        case "code", "cursor", "zed":
            // `code -g file:line[:col]`; Zed accepts the same `file:line:col`.
            var location = "\(path):\(line)"
            if let column { location += ":\(column)" }
            return editor == "zed" ? [location] : ["-g", location]
        case "idea", "webstorm":
            var args = ["--line", "\(line)"]
            if let column { args += ["--column", "\(column)"] }
            return args + [path]
        default:
            return [path]
        }
    }

    /// Blocking launch helper; returns an error message or nil on success.
    private nonisolated static func openInEditorImpl(
        editor: String,
        path: String,
        line: Int?,
        column: Int?
    ) -> String? {
        guard FileManager.default.fileExists(atPath: path) else {
            return "Not found: \(path)"
        }

        let args = editorOpenArguments(editor: editor, path: path, line: line, column: column)

        func run(_ executable: String, _ args: [String]) -> String? {
            let process = Process()
            process.executableURL = URL(fileURLWithPath: executable)
            process.arguments = args
            process.standardInput = FileHandle.nullDevice
            process.standardOutput = FileHandle.nullDevice
            let stderr = Pipe()
            process.standardError = stderr
            do {
                try process.run()
            } catch {
                return error.localizedDescription
            }
            let errData = stderr.fileHandleForReading.readDataToEndOfFile()
            process.waitUntilExit()
            if process.terminationStatus == 0 { return nil }
            let message = (String(data: errData, encoding: .utf8) ?? "")
                .trimmingCharacters(in: .whitespacesAndNewlines)
            return message.isEmpty ? "\(executable) exited with status \(process.terminationStatus)" : message
        }

        for cli in bundledEditorCLIs(editor)
        where FileManager.default.isExecutableFile(atPath: cli) {
            return run(cli, args)
        }
        if let app = fallbackEditorApp(editor) {
            // `open -a` can't pass a line; only the path survives. The bundled
            // CLI (above) is what carries line/column for code/cursor.
            if run("/usr/bin/open", ["-a", app, path]) == nil { return nil }
            // App launch failed → try the editor id as a CLI before giving up.
            return run("/usr/bin/env", [editor] + args)
        }
        return run("/usr/bin/env", [editor] + args)
    }

    // MARK: - Open workspace target (titlebar dropdown)

    func openWorkspace(path: String, in target: WorkspaceOpenTarget) {
        if target == .finder {
            revealInFinder(path: path)
            return
        }

        Task {
            let error = await Task.detached(priority: .userInitiated) {
                Self.openWorkspaceImpl(target: target, path: path)
            }.value
            if let error {
                await MainActor.run {
                    Self.showErrorAlert(
                        title: "Couldn't open \(target.title)",
                        message: error
                    )
                }
            }
        }
    }

    private nonisolated static func openWorkspaceImpl(
        target: WorkspaceOpenTarget,
        path: String
    ) -> String? {
        guard FileManager.default.fileExists(atPath: path) else {
            return "Folder not found: \(path)"
        }

        func run(_ args: [String]) -> String? {
            let process = Process()
            process.executableURL = URL(fileURLWithPath: "/usr/bin/open")
            process.arguments = args
            process.standardInput = FileHandle.nullDevice
            process.standardOutput = FileHandle.nullDevice
            let stderr = Pipe()
            process.standardError = stderr
            do {
                try process.run()
            } catch {
                return error.localizedDescription
            }
            let errData = stderr.fileHandleForReading.readDataToEndOfFile()
            process.waitUntilExit()
            if process.terminationStatus == 0 { return nil }
            let message = (String(data: errData, encoding: .utf8) ?? "")
                .trimmingCharacters(in: .whitespacesAndNewlines)
            return message.isEmpty ? "open exited with status \(process.terminationStatus)" : message
        }

        var lastError: String?
        for bundleID in target.bundleIdentifiers {
            if let error = run(["-b", bundleID, path]) {
                lastError = error
            } else {
                return nil
            }
        }
        for app in target.appNames {
            if let error = run(["-a", app, path]) {
                lastError = error
            } else {
                return nil
            }
        }
        return lastError ?? "No launch target found for \(target.title)."
    }

    // MARK: - Worktrees (create/remove; worktree.rs via WorktreeGit)

    /// "New worktree…" (project-menu-create-workspace → NewWorkspaceView in
    /// the Svelte app). The native stand-in is a dialog with a branch combo
    /// box (pick an existing local branch to check it out, or type a new
    /// name) plus a base-branch popup showing what a new branch forks from
    /// (default: the repo's mainline — `origin/<default>` when there is a
    /// remote, else local main/master, else the current branch).
    func promptCreateWorktree(projectID: String) {
        guard let project = displayProjectsByID[projectID] else { return }
        let repoPath = project.path
        Task { [weak self] in
            // Branch enumeration shells out to git; keep it off the main actor.
            let info = await Task.detached(priority: .userInitiated) {
                (all: WorktreeGit.listBranches(repoPath: repoPath),
                 remote: WorktreeGit.listRemoteBranches(repoPath: repoPath),
                 current: WorktreeGit.currentBranch(repoPath: repoPath),
                 defaultBase: WorktreeGit.defaultBaseRef(repoPath: repoPath),
                 checkedOut: WorktreeGit.checkedOutBranches(repoPath: repoPath))
            }.value
            self?.showCreateWorktreeDialog(
                project: project,
                branches: info.all,
                remoteBranches: info.remote,
                currentBranch: info.current,
                defaultBase: info.defaultBase,
                checkedOutBranches: info.checkedOut
            )
        }
    }

    private func showCreateWorktreeDialog(
        project: Project,
        branches: [String],
        remoteBranches: [String],
        currentBranch: String?,
        defaultBase: String?,
        checkedOutBranches: Set<String>
    ) {
        let alert = NSAlert()
        alert.messageText = "New worktree"
        alert.informativeText = """
        Give the worktree a readable name for the sidebar and folder, then \
        pick an existing branch or type a new branch name. If name is blank, \
        the branch name is used.
        """

        let width: CGFloat = 320
        let labelWidth: CGFloat = 64
        let fieldWidth = width - labelWidth
        // Locals first (recency-sorted), then remote-tracking refs; the
        // popup pre-selects the mainline so new branches fork from it
        // instead of whatever the main checkout happens to have open.
        let baseChoices = branches + remoteBranches.filter { !branches.contains($0) }
        let hasBasePicker = !baseChoices.isEmpty
        let nameY: CGFloat = hasBasePicker ? 72 : 38
        let branchY: CGFloat = hasBasePicker ? 36 : 0

        let nameLabel = NSTextField(labelWithString: "Name")
        nameLabel.font = .systemFont(ofSize: NSFont.smallSystemFontSize)
        nameLabel.textColor = .secondaryLabelColor
        nameLabel.alignment = .right
        nameLabel.frame = NSRect(x: 0, y: nameY + 5, width: labelWidth - 8, height: 17)

        let nameField = NSTextField(frame: NSRect(
            x: labelWidth, y: nameY, width: fieldWidth, height: 24
        ))
        nameField.placeholderString = "Plugin refactor"

        let branchLabel = NSTextField(labelWithString: "Branch")
        branchLabel.font = .systemFont(ofSize: NSFont.smallSystemFontSize)
        branchLabel.textColor = .secondaryLabelColor
        branchLabel.alignment = .right
        branchLabel.frame = NSRect(x: 0, y: branchY + 5, width: labelWidth - 8, height: 17)

        // Branches checked out in another worktree can't be checked out
        // again, but any branch can be the base of a new one.
        let available = branches.filter { !checkedOutBranches.contains($0) }
        let combo = NSComboBox(frame: NSRect(
            x: labelWidth, y: branchY, width: fieldWidth, height: 26
        ))
        combo.placeholderString = "feature/plugin-refactor"
        combo.completes = true
        combo.addItems(withObjectValues: available)

        let baseLabel = NSTextField(labelWithString: "Start from")
        baseLabel.font = .systemFont(ofSize: NSFont.smallSystemFontSize)
        baseLabel.textColor = .secondaryLabelColor
        baseLabel.alignment = .right
        baseLabel.frame = NSRect(x: 0, y: 5, width: labelWidth - 8, height: 17)

        let basePopup = NSPopUpButton(
            frame: NSRect(x: labelWidth, y: 0, width: fieldWidth, height: 25),
            pullsDown: false
        )
        basePopup.addItems(withTitles: baseChoices)
        if let defaultBase, baseChoices.contains(defaultBase) {
            basePopup.selectItem(withTitle: defaultBase)
        } else if let currentBranch {
            basePopup.selectItem(withTitle: currentBranch)
        }

        let container = NSView(frame: NSRect(
            x: 0, y: 0, width: width, height: hasBasePicker ? 98 : 64
        ))
        container.addSubview(nameLabel)
        container.addSubview(nameField)
        container.addSubview(branchLabel)
        container.addSubview(combo)
        if hasBasePicker {
            container.addSubview(baseLabel)
            container.addSubview(basePopup)
        }

        alert.accessoryView = container
        alert.addButton(withTitle: "Create Worktree")
        alert.addButton(withTitle: "Cancel")
        alert.window.initialFirstResponder = combo
        guard alert.runModal() == .alertFirstButtonReturn else { return }
        let branch = combo.stringValue.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !branch.isEmpty else { return }
        let name = nameField.stringValue.trimmingCharacters(in: .whitespacesAndNewlines)
        let baseRef = hasBasePicker ? basePopup.titleOfSelectedItem : nil
        createWorktree(
            parentProject: project,
            branch: branch,
            name: name.isEmpty ? nil : name,
            baseRef: baseRef
        )
    }

    private func createWorktree(
        parentProject: Project, branch: String, name: String?, baseRef: String?
    ) {
        let repoPath = parentProject.path
        Task { [weak self] in
            let result = await Task.detached(priority: .userInitiated) {
                WorktreeGit.createWorktree(
                    repoPath: repoPath, branch: branch, baseRef: baseRef, folderName: name
                )
            }.value
            await MainActor.run {
                guard let self else { return }
                switch result {
                case .created(let path):
                    if self.selectedHostScope == .local {
                        self.registerWorktreeProject(
                            parentID: parentProject.id, path: path, branch: branch, name: name
                        )
                    } else if let home = self.scopedStateHome {
                        // Scoped workspace: the child project record lands in
                        // ITS app-state.json (the Add Project write class);
                        // its host lists it on the next bootstrap.
                        self.registerScopedWorktreeProject(
                            parentID: parentProject.id,
                            path: path,
                            branch: branch,
                            name: name,
                            home: home
                        )
                    }
                case .failed(let message):
                    Self.showErrorAlert(title: "Couldn't create worktree", message: message)
                }
            }
        }
    }

    /// Scoped twin of `registerWorktreeProject`: append the worktree child
    /// row to the WORKSPACE's own app-state.json under the shared lock. Not
    /// the `native-` id prefix — that namespace is each instance's own
    /// UserDefaults-mirrored records.
    private func registerScopedWorktreeProject(
        parentID: String, path: String, branch: String, name: String?, home: URL
    ) {
        let childID = "wt-" + UUID().uuidString.lowercased()
        let stateURL = home.appendingPathComponent("app-state.json")
        let wrote = PresetStateFile.edit(at: stateURL) { object in
            var projects = (object["projects"] as? [[String: Any]]) ?? []
            guard !projects.contains(where: {
                ($0["parent_project_id"] as? String) == parentID
                    && ($0["worktree_branch"] as? String) == branch
            }) else { return }
            let maxSort = projects.compactMap { $0["sort_order"] as? Int }.max() ?? 0
            projects.append([
                "id": childID,
                "name": name?.isEmpty == false ? name! : branch,
                "path": path,
                "parent_project_id": parentID,
                "worktree_branch": branch,
                "sort_order": maxSort + 1,
            ])
            object["projects"] = projects
        }
        if !wrote {
            Self.showErrorAlert(
                title: "Couldn't create worktree",
                message: "Failed to register the worktree in the workspace's state."
            )
        }
    }

    // MARK: - Experimental features (Settings ▸ Experimental)

    /// Whether an experimental feature is active for this store. Reads the
    /// published set so SwiftUI views that gate on it recompute when it flips.
    func isExperimentalEnabled(_ feature: ExperimentalFeature) -> Bool {
        enabledExperimentalKeys.contains(feature.key)
    }

    /// Toggle an experimental feature: persist the preference and update the
    /// published set (which republishes the store so dependent UI re-evaluates).
    func setExperimental(_ enabled: Bool, for feature: ExperimentalFeature) {
        UnpeelFeatureFlags.setEnabled(enabled, for: feature)
        if enabled {
            enabledExperimentalKeys.insert(feature.key)
        } else {
            enabledExperimentalKeys.remove(feature.key)
        }
        if feature == .computerUse {
            // The Rust launch gate (`computer_mcp::requested_from_app_state`)
            // reads `experimental_features.computer_use` from app-state.json;
            // the defaults overlay alone never reached it. Same locked,
            // announcing write path as every other shared-state edit.
            if Self.writeComputerUseExperiment(enabled, appStateFile: LaunchConfig.appStateFile) {
                announceStateChange("app-state")
            }
        }
    }

    /// Whether the selected Host advertises a computer-use adapter in its
    /// bootstrap. Drives Controller-side visibility for non-local scopes
    /// (decision D2); the local Mac scope never reads it.
    var selectedHostAdvertisesComputerUse: Bool {
        guard selectedHostScope != .local else { return false }
        return UnpeelFeatureFlags.computerUseControllable(
            hostAdvertisesAvailability: remoteHostRuntime.snapshot?
                .workspaceSettings?.experimentalSettings?.computerUseAvailable
        )
    }

    /// Write `experimental_features.computer_use` into an `app-state.json`
    /// under the shared cross-process lock, keeping every sibling gate and
    /// unknown key. Returns whether the file was written.
    @discardableResult
    nonisolated static func writeComputerUseExperiment(_ enabled: Bool, appStateFile: URL) -> Bool {
        PresetStateFile.edit(at: appStateFile) { object in
            var features = object["experimental_features"] as? [String: Any] ?? [:]
            features["computer_use"] = enabled
            object["experimental_features"] = features
        }
    }

    /// ensure_worktree_project parity (project.rs:102-160): the worktree
    /// becomes a child project named after its custom name or branch so it groups sessions
    /// and reuses all project UI. Stored natively; never written to
    /// app-state.json.
    @discardableResult
    func registerWorktreeProject(
        parentID: String, path: String, branch: String, name: String? = nil
    ) -> String? {
        let canonicalPath = URL(fileURLWithPath: path).resolvingSymlinksInPath().path
        let trimmedName = name?.trimmingCharacters(in: .whitespacesAndNewlines)
        let projectName = trimmedName?.isEmpty == false ? trimmedName! : branch
        var projectID = projectsByID.values.first {
            URL(fileURLWithPath: $0.path).resolvingSymlinksInPath().path == canonicalPath
        }?.id
        if projectID == nil {
            var records = loadNativeProjects()
            projectID = records.first {
                URL(fileURLWithPath: $0.path).resolvingSymlinksInPath().path == canonicalPath
            }?.id
            if projectID == nil {
                let id = "native-\(UUID().uuidString.lowercased())"
                records.append(NativeProjectRecord(
                    id: id,
                    name: projectName,
                    path: path,
                    parentProjectID: parentID,
                    worktreeBranch: branch
                ))
                projectID = id
                if let data = try? JSONEncoder().encode(records) {
                    AppDefaults.shared.set(data, forKey: Self.nativeProjectsKey)
                    mirrorProjectsToSharedState()
                }
            }
        }
        // Show the result like handleWorkspacesEnabled (Sidebar.svelte:131):
        // expand the parent so the new inline worktree folder row is visible
        // (the row itself lands via rescan).
        expandedProjectIDs.insert(parentID)
        rescan()
        return projectID ?? projectsByID.values.first {
            URL(fileURLWithPath: $0.path).resolvingSymlinksInPath().path == canonicalPath
        }?.id
    }

    /// Rename the Unpeel worktree project. This deliberately does not rename
    /// the git branch or move an existing checkout folder; live sessions and
    /// saved manifests continue to point at the same path.
    func promptRenameWorktreeProject(_ projectID: String) {
        guard let project = projectsByID[projectID],
              project.parentProjectID != nil
        else { return }
        let branch = project.worktreeBranch

        let alert = NSAlert()
        alert.messageText = branch != nil ? "Rename worktree" : "Rename group"
        alert.informativeText = branch.map {
            """
            This changes the name shown in Unpeel. The git branch stays \
            "\($0)".
            """
        } ?? "This changes the name shown in Unpeel."
        let field = NSTextField(frame: NSRect(x: 0, y: 0, width: 280, height: 24))
        field.stringValue = project.name
        field.placeholderString = branch ?? project.name
        alert.accessoryView = field
        alert.addButton(withTitle: "Rename")
        alert.addButton(withTitle: "Cancel")
        alert.window.initialFirstResponder = field
        guard alert.runModal() == .alertFirstButtonReturn else { return }
        if project.acceptsSessionDrop {
            // Plain groups can live only in the shared file (TUI-created,
            // `tui-` ids) — the worktree path's native-record lookup would
            // silently drop the rename for those.
            renameGroupProject(projectID, to: field.stringValue)
        } else {
            renameWorktreeProject(projectID, to: field.stringValue)
        }
    }

    private func renameWorktreeProject(_ projectID: String, to name: String) {
        let trimmed = name.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !trimmed.isEmpty,
              let project = projectsByID[projectID],
              project.parentProjectID != nil,
              trimmed != project.name
        else { return }

        let canonicalPath = URL(fileURLWithPath: project.path).resolvingSymlinksInPath().path
        var records = loadNativeProjects()
        // Groups share the parent's path, so the path fallback (worktree
        // records whose id drifted) only applies to branch-backed children.
        guard let index = records.firstIndex(where: { $0.id == projectID })
            ?? (project.worktreeBranch != nil
                ? records.firstIndex(where: {
                    URL(fileURLWithPath: $0.path).resolvingSymlinksInPath().path == canonicalPath
                })
                : nil)
        else { return }
        records[index].name = trimmed
        if let data = try? JSONEncoder().encode(records) {
            AppDefaults.shared.set(data, forKey: Self.nativeProjectsKey)
            mirrorProjectsToSharedState()
        }
        rescan()
    }

    // MARK: - Groups (plain organizational child folders)

    /// "New group…" on a project: name prompt, then a child project record
    /// with the parent's path, no branch, `isFolder` set. Groups render as
    /// inline folder rows beside the worktrees and exist purely to organize
    /// sessions — moving a session in is a `project-override.json` marker,
    /// never a manifest edit.
    func promptCreateGroup(projectID: String) {
        guard let project = projectsByID[projectID] else { return }
        let alert = NSAlert()
        alert.messageText = "New group"
        alert.informativeText = "Group sessions under a named folder in the sidebar. Sessions keep running where they are — this only organizes the list."
        let field = NSTextField(frame: NSRect(x: 0, y: 0, width: 280, height: 24))
        field.placeholderString = "Research"
        alert.accessoryView = field
        alert.addButton(withTitle: "Create")
        alert.addButton(withTitle: "Cancel")
        alert.window.initialFirstResponder = field
        guard alert.runModal() == .alertFirstButtonReturn else { return }
        let name = field.stringValue.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !name.isEmpty else { return }
        var records = loadNativeProjects()
        records.append(NativeProjectRecord(
            id: "native-\(UUID().uuidString.lowercased())",
            name: name,
            path: project.path,
            parentProjectID: projectID,
            isFolder: true
        ))
        if let data = try? JSONEncoder().encode(records) {
            AppDefaults.shared.set(data, forKey: Self.nativeProjectsKey)
            mirrorProjectsToSharedState()
        }
        expandedProjectIDs.insert(projectID)
        rescan()
    }

    /// Rename either a native-record group or a shared-file group. The latter
    /// is how a group created in the TUI remains editable while the app is
    /// running; native records still mirror their updated name into the file.
    @discardableResult
    func renameGroupProject(_ projectID: String, to rawName: String) -> Bool {
        let name = rawName.trimmingCharacters(in: .whitespacesAndNewlines)
        let project = nativeHostAdapterEffectDepth > 0
            ? projectsByID[projectID]
            : displayProjectsByID[projectID]
        guard !name.isEmpty,
              let project,
              project.acceptsSessionDrop
        else { return false }
        if routesProjectVerbThroughHost(projectID) {
            performRemoteVerb("Couldn't rename the group") { runtime in
                try await runtime.renameProjectGroup(
                    projectID: projectID,
                    displayName: name
                )
            }
            return true
        }
        var records = loadNativeProjects()
        if let index = records.firstIndex(where: { $0.id == projectID }) {
            records[index].name = name
            guard let data = try? JSONEncoder().encode(records) else { return false }
            AppDefaults.shared.set(data, forKey: Self.nativeProjectsKey)
            mirrorProjectsToSharedState()
        } else {
            let changed = editPresetStateAnnouncing { object in
                var projects = (object["projects"] as? [[String: Any]]) ?? []
                guard let index = projects.firstIndex(where: {
                    ($0["id"] as? String) == projectID
                        && ($0["is_folder"] as? Bool) == true
                        && $0["parent_project_id"] is String
                        && $0["worktree_branch"] == nil
                }) else { return }
                projects[index]["name"] = name
                object["projects"] = projects
            }
            guard changed else { return false }
        }
        rescan()
        return true
    }

    /// Remove a group: unpin and rehome every session under its parent, then
    /// archive only the ones with real resume state. A failed/empty agent
    /// launch remains visible under the parent rather than being mislabeled
    /// as a recoverable archive or deleted as a side effect of group removal.
    /// Controller callers already confirmed and pass `confirm: false`.
    @discardableResult
    func removeGroupProject(_ projectID: String, confirm: Bool = true) -> Int? {
        guard let project = projectsByID[projectID],
              let parentID = project.parentProjectID,
              project.acceptsSessionDrop
        else { return nil }
        let members = sessionsByID.values
            .filter { effectiveProjectID(for: $0) == projectID }
        if confirm && !members.isEmpty {
            let count = members.count
            let noun = count == 1 ? "session" : "sessions"
            let parentName = projectsByID[parentID]?.name ?? "the parent project"
            let alert = NSAlert()
            alert.messageText = "Remove group?"
            alert.informativeText = "\(count) \(noun) will move under \(parentName). Resumable conversations will also be archived."
            alert.addButton(withTitle: "Remove Group")
            alert.addButton(withTitle: "Cancel")
            guard alert.runModal() == .alertFirstButtonReturn else { return nil }
        }
        var archivedCount = 0
        for session in members {
            unpinSession(projectID: projectID, sessionID: session.id)
            moveSession(session.id, toProjectID: parentID)
            if sessionCanArchive(session.id) {
                archiveSession(session.id)
                archivedCount += 1
            }
        }
        var records = loadNativeProjects()
        if records.contains(where: { $0.id == projectID }) {
            records.removeAll { $0.id == projectID }
            if let data = try? JSONEncoder().encode(records) {
                AppDefaults.shared.set(data, forKey: Self.nativeProjectsKey)
                mirrorProjectsToSharedState()
            }
        } else {
            editPresetStateAnnouncing { object in
                var projects = (object["projects"] as? [[String: Any]]) ?? []
                projects.removeAll { ($0["id"] as? String) == projectID }
                object["projects"] = projects
            }
        }
        rescan()
        return archivedCount
    }

    /// Whether a session row in a DATE-SORTED list still participates in the
    /// detached drag. The lift is how a Session leaves a group, and a
    /// committed within-list reorder flips the list to custom order through
    /// local state or `project.organization.set`, as appropriate. A scoped
    /// Session may also lift solely to reach the Controller-owned pane edges.
    func canLiftSessionFromDateSortedList(_ sessionID: String) -> Bool {
        remoteSummariesByID[sessionID] == nil
            || selectedHostScope.supportsSessionPanes
    }

    // MARK: - Session panes (Controller-window presentation)

    /// One runtime mark in the collapsed sidebar representation of a pane
    /// group. Status travels with the mark so the carrying row can aggregate
    /// activity over every pane. The representative is only the row that
    /// carries the group; it has no special layout, lifecycle, or width
    /// semantics.
    struct PaneSidebarItem: Identifiable, Equatable {
        let paneID: String
        let sessionID: String
        let command: String
        let agentName: String
        let status: SessionStatus
        let isRepresentative: Bool
        var id: String { paneID }
    }

    /// Shared collapsed-row mark builder. The live sidebar and the
    /// workspace-swipe preview must derive identical runtime labels from the
    /// same Session entry even though they read different pane-layout scopes.
    static func paneSidebarItem(
        paneID: String,
        session: SessionEntry,
        isRepresentative: Bool
    ) -> PaneSidebarItem {
        let command = session.presentationCommand
        let runtimeName = UnpeelRuntimeCatalog.runtime(command: command)?.label
            ?? (command.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty
                ? "Terminal"
                : command.split(whereSeparator: \.isWhitespace).first.map(String.init)
                    ?? "Terminal")
        let title = session.label.trimmingCharacters(in: .whitespacesAndNewlines)
        return PaneSidebarItem(
            paneID: paneID,
            sessionID: session.id,
            command: command,
            agentName: title.isEmpty ? runtimeName : title,
            status: session.status,
            isRepresentative: isRepresentative
        )
    }

    var paneLayoutState: PaneLayoutState {
        paneLayoutController.state
    }

    var terminalPaneDropTarget: PaneDropTarget? {
        paneLayoutController.dropTarget
    }

    var terminalPaneFocusRequest: PaneLayoutController.FocusRequest? {
        paneLayoutController.focusRequest
    }

    var activeTerminalPane: PaneLayoutController.ActivePane? {
        paneLayoutController.activePane
    }

    var zoomedTerminalPane: PaneLayoutController.ZoomedPane? {
        paneLayoutController.zoomedPane
    }

    func setTerminalPaneDropTarget(_ target: PaneDropTarget?) {
        paneLayoutController.setDropTarget(target)
    }

    /// Full-pane highlight while a pane-title drag hovers another pane in the
    /// same group: shows where the dragged pane lands; the swap itself commits
    /// on DROP (never live mid-drag).
    @Published private(set) var paneSwapPreviewPaneID: String?

    /// A launcher keeps its pane identity and geometry while its Host create
    /// is in flight. The view swaps only the picker content for a spinner.
    @Published private(set) var pendingPaneLaunchIDs: Set<String> = []

    func paneLaunchIsPending(_ paneID: String) -> Bool {
        pendingPaneLaunchIDs.contains(paneID)
    }

    func setPaneSwapPreview(_ paneID: String?) {
        if paneSwapPreviewPaneID != paneID {
            paneSwapPreviewPaneID = paneID
        }
    }

    /// Same preview for the right panel's stack: the hovered member highlights
    /// and the reorder commits on drop. Keyed by session id (panel panes are
    /// synthetic solo panes).
    @Published private(set) var projectSidebarReorderPreviewID: String?

    func setProjectSidebarReorderPreview(_ sessionID: String?) {
        if projectSidebarReorderPreviewID != sessionID {
            projectSidebarReorderPreviewID = sessionID
        }
    }

    /// Insert preview while a NON-member drag hovers a panel pane: the half
    /// of the hovered pane where the pinned session will land (above or
    /// below), mirroring the main area's edge-split previews.
    struct ProjectSidebarInsertPreview: Equatable {
        let sessionID: String
        let below: Bool
    }

    @Published private(set) var projectSidebarInsertPreview: ProjectSidebarInsertPreview?

    func setProjectSidebarInsertPreview(_ preview: ProjectSidebarInsertPreview?) {
        if projectSidebarInsertPreview != preview {
            projectSidebarInsertPreview = preview
        }
    }

    func setActiveTerminalPane(
        groupID: String,
        paneID: String,
        sessionID: String?
    ) {
        let before = observedSessionID
        paneLayoutController.setActivePane(
            groupID: groupID,
            paneID: paneID,
            sessionID: sessionID
        )
        if observedSessionID != before {
            handleObservationChanged()
        }
    }

    func clearActiveTerminalPane() {
        let before = observedSessionID
        paneLayoutController.clearActivePane()
        if observedSessionID != before {
            handleObservationChanged()
        }
    }

    /// The solo / project-sidebar pane that last took focus. Solo and panel
    /// panes clear `activeTerminalPane` (there is nothing to disambiguate
    /// WITHIN a one-pane group), so this is what the white focus border keys
    /// on when the main area holds a single pane and the project sidebar adds
    /// more panes beside it.
    @Published private(set) var focusedSoloSessionID: String?

    func setFocusedSoloSession(_ sessionID: String?) {
        if focusedSoloSessionID != sessionID { focusedSoloSessionID = sessionID }
    }

    @discardableResult
    func consumeTerminalPaneFocus(
        _ request: PaneLayoutController.FocusRequest
    ) -> Bool {
        paneLayoutController.consumeFocusRequest(request)
    }

    @discardableResult
    private func mutatePaneLayout<T>(
        _ mutation: (inout PaneLayoutState) throws -> T
    ) -> T? {
        do {
            let before = paneLayoutController.state
            let result = try paneLayoutController.mutate(mutation)
            invalidateSidebarLists()
            if paneLayoutController.state != before {
                announceStateChange("pane-layouts")
                clearZoomIfStructureChanged(before: before)
            }
            return result
        } catch {
            return nil
        }
    }

    /// Zoom survives geometry-only changes (resize, equalize) but any
    /// structural change in the zoomed group — insert, detach, swap, close —
    /// un-zooms so hidden siblings can never change shape invisibly.
    private func clearZoomIfStructureChanged(before: PaneLayoutState) {
        guard let zoomed = paneLayoutController.zoomedPane else { return }
        let old = before.group(id: zoomed.groupID)?.root.structuralIdentity
        let new = paneLayoutState.group(id: zoomed.groupID)?.root.structuralIdentity
        if old != new {
            paneLayoutController.clearZoom()
        }
    }

    /// Whether dropping a Session on the content edge can add it to the
    /// currently displayed pane group. A Session can appear in at most one
    /// pane group in a Controller window, and groups never merge implicitly.
    func canSplitTerminal(with draggedID: String) -> Bool {
        guard selectedHostScope.supportsSessionPanes,
              !settingsVisible,
              archivedProjectID == nil,
              !recentActivityVisible,
              let shownID = selectedSessionID,
              shownID != draggedID,
              let shown = displaySessionsByID[shownID],
              let dragged = displaySessionsByID[draggedID],
              shown.isAttachable,
              dragged.isAttachable,
              paneLayoutState.location(ofSession: draggedID) == nil
        else { return false }

        if let group = paneLayoutState.group(containingSession: shownID) {
            let prospective = group.root.sessionLeaves.count
                + (group.root.containsLauncher ? 1 : 0)
            return prospective < PaneLayoutState.maximumSessionLeafCount
        }
        return true
    }

    /// Split a visible pane (or the group's edge) with another Session. This
    /// changes only Controller view state; neither Session is moved,
    /// restarted, or otherwise mutated — except a right-panel member, which
    /// un-files back to its project first (its surface leaves the panel for
    /// the main split; it cannot mount in both).
    func splitTerminal(with addedSessionID: String, near target: PaneDropTarget?) {
        guard canSplitTerminal(with: addedSessionID),
              let shownID = selectedSessionID
        else { return }
        if sessionIsInProjectSidebar(addedSessionID) {
            moveSessionToMainArea(addedSessionID)
        }

        let location = mutatePaneLayout { state -> PaneLocation in
            switch target {
            case let .pane(paneID, edge):
                return try state.insertSession(
                    addedSessionID, splitting: paneID, edge: edge
                )
            case let .groupEdge(edge):
                if let group = state.group(containingSession: shownID) {
                    return try state.insertSession(
                        addedSessionID, atGroupEdge: edge, of: group.id
                    )
                }
                return try state.insertSession(
                    addedSessionID, beside: shownID, at: edge
                )
            case nil:
                return try state.insertSession(
                    addedSessionID, beside: shownID, at: .right
                )
            }
        }
        guard let location else { return }

        paneLayoutController.setPendingReveal(
            groupID: location.groupID,
            paneID: location.paneID
        )
    }

    /// The menu-driven launcher is local-only because choosing a preset
    /// creates a Session. Existing-session panes work in every Host scope.
    func canOpenPaneLauncher() -> Bool {
        guard !settingsVisible,
              archivedProjectID == nil,
              !recentActivityVisible,
              let shownID = selectedSessionID,
              let shown = displaySessionsByID[shownID],
              shown.isAttachable,
              // A panel member never mounts in the main area — its ⌘D routes
              // to the panel launcher (`canOpenProjectSidebarLauncher`).
              !sessionIsInProjectSidebar(shownID),
              // Panes are pure Controller view state, so the launcher works
              // in EVERY scope; the only Host-side requirement is that
              // choosing a preset can create a Session there (local always,
              // scoped workspaces/Hosts via the advertised create operation).
              canCreateSessions(inProject: shown.projectID)
        else { return false }

        guard let group = paneLayoutState.group(containingSession: shownID) else {
            return true
        }
        let prospective = group.root.sessionLeaves.count
            + (group.root.containsLauncher ? 1 : 0)
        return prospective < PaneLayoutState.maximumSessionLeafCount
            && !group.root.containsLauncher
    }

    /// Session ▸ Split Pane: add a transient launcher by splitting a pane.
    /// Defaults to the active pane (falling back to the shown session's pane)
    /// on the requested edge.
    func openPaneLauncher(at edge: PaneEdge = .right, splitting paneID: String? = nil) {
        guard canOpenPaneLauncher(),
              let shownID = selectedSessionID,
              let shown = displaySessionsByID[shownID]
        else { return }

        // Remote/workspace rows already carry their effective project; the
        // local override resolution only applies to this instance's tree.
        let projectID: String
        if selectedHostScope == .local {
            let effectiveID = effectiveProjectID(for: shown)
            projectID = projectsByID[effectiveID] == nil
                ? shown.projectID
                : effectiveID
        } else {
            projectID = shown.projectID
        }
        let targetPaneID = paneID
            ?? paneLayoutController.activePane?.paneID
            ?? paneLayoutState.location(ofSession: shownID)?.paneID
        guard let location = mutatePaneLayout({ state -> PaneLocation in
            if let targetPaneID, state.location(ofPane: targetPaneID) != nil {
                return try state.insertLauncher(
                    projectID: projectID,
                    splitting: targetPaneID,
                    edge: edge
                )
            }
            return try state.insertLauncher(
                projectID: projectID,
                beside: shownID,
                at: edge
            )
        }) else { return }

        paneLayoutController.setPendingReveal(
            groupID: location.groupID,
            paneID: location.paneID
        )
    }

    /// Launch a Session into an existing launcher slot. The Pane keeps its
    /// stable identity while its content changes from launcher to Session.
    func launchSessionIntoPane(
        representativeSessionID: String,
        launcherPaneID: String,
        preset: Preset
    ) {
        // Scoped workspace/Host: the launcher rides the same create verb the
        // sidebar "+" uses, then binds the pane once the row lands.
        launchRemoteSessionIntoPane(launcherPaneID: launcherPaneID, preset: preset)
    }

    /// Scoped-workspace/Host twin of the local launcher bind: create the
    /// Session over the protocol (capability-gated `session.create`), wait
    /// for the bootstrap to list the row (binding earlier would transiently
    /// collapse the group's presentation to solo), then bind the launcher
    /// pane. The pane tree itself is Controller view state in every scope.
    private func launchRemoteSessionIntoPane(launcherPaneID: String, preset: Preset) {
        guard let location = paneLayoutState.location(ofPane: launcherPaneID),
              let group = paneLayoutState.groups.first(where: {
                  $0.id == location.groupID
              }),
              let launcher = group.panes.first(where: { $0.id == launcherPaneID }),
              case let .launcher(projectID) = launcher.content,
              !pendingPaneLaunchIDs.contains(launcherPaneID)
        else { return }
        let presetID = preset.command.isEmpty ? nil : preset.id
        pendingPaneLaunchIDs.insert(launcherPaneID)
        performRemoteVerb("Couldn't start the session", onFailure: { [weak self] in
            self?.pendingPaneLaunchIDs.remove(launcherPaneID)
        }) { [weak self] runtime in
            let sessionID = try await runtime.createSession(
                projectID: projectID,
                presetID: presetID,
                command: presetID == nil ? preset.command : nil,
                selectOnCreate: false
            )
            guard let self else { return }
            // Current Hosts return a complete correlated starting row. Project
            // it synchronously so this launcher binds on the create receipt,
            // not on the next polling tick. Older Hosts retain the bounded
            // snapshot fallback below.
            self.projectRemoteScope(snapshot: runtime.snapshot)
            runtime.requestImmediateRefresh()
            for _ in 0..<30 where self.displaySessionsByID[sessionID] == nil {
                try await Task.sleep(nanoseconds: 100_000_000)
            }
            guard self.displaySessionsByID[sessionID] != nil,
                  self.paneLayoutState.location(ofPane: launcherPaneID) != nil
            else {
                throw RemoteHostVerbError(
                    operation: "new pane Session",
                    message: "The Host created the Session but has not published it yet.",
                    outcomeIsUnknown: false
                )
            }
            _ = self.mutatePaneLayout { state in
                try state.bindLauncher(launcherPaneID, toSessionID: sessionID)
            }
            self.pendingPaneLaunchIDs.remove(launcherPaneID)
            self.setActiveTerminalPane(
                groupID: location.groupID,
                paneID: launcherPaneID,
                sessionID: sessionID
            )
        }
    }

    /// A launcher is a transient interaction, not durable layout content.
    /// Leaving the group removes it; a one-Session remainder dissolves.
    private func dismissPaneLaunchers(containing sessionID: String) {
        guard let group = paneLayoutState.group(containingSession: sessionID),
              let launcher = group.panes.first(where: {
                  $0.content.isLauncher
              })
        else { return }
        _ = mutatePaneLayout { state in
            try state.removeLauncher(launcher.id)
        }
        if paneLayoutController.activePane?.paneID == launcher.id {
            clearActiveTerminalPane()
        }
        paneLayoutController.clearPendingReveal()
    }

    func pendingPaneReveal(in groupID: String) -> String? {
        paneLayoutController.pendingRevealPaneID(forGroupID: groupID)
    }

    func consumePaneReveal(groupID: String, paneID: String) -> Bool {
        paneLayoutController.consumePendingReveal(
            groupID: groupID,
            paneID: paneID
        )
    }

    /// Session ids hidden beneath each valid pane group's representative row.
    var activePaneMemberSessionIDs: Set<String> {
        var hidden = Set<String>()
        for group in paneLayoutState.groups {
            guard let representativeID = group.representativeSessionID,
                  let valid = validatedPaneGroup(
                      containingSession: representativeID
                  ),
                  let validRepresentative = valid.representativeSessionID
            else { continue }
            hidden.formUnion(valid.sessionIDs.filter {
                $0 != validRepresentative
            })
        }
        return hidden
    }

    private func reconcilePaneLayout() {
        guard selectedHostScope == .local else { return }
        reconcilePaneLayout(eligibleSessionIDs: Set(
            sessionsByID.values.compactMap { session in
                guard !archivedSessionIDs.contains(session.id),
                      !removingSessionIDs.contains(session.id),
                      session.isAttachable || session.status == .starting
                else { return nil }
                return session.id
            }
        ))
    }

    /// A reveal revision is an intent edge from the Host, not persistent pane
    /// membership. Recording the handled revision in this Controller's own
    /// defaults makes a user detach/dismiss durable: rescans do not reinsert
    /// the panel until a later `apps.open` advances the revision.
    private func appPresentationReceipt(presentationID: String) -> UInt64 {
        let key = "\(paneLayoutController.scopeID)|\(presentationID)"
        let receipts = AppDefaults.shared.dictionary(
            forKey: Self.appPresentationReceiptsKey
        ) ?? [:]
        return (receipts[key] as? NSNumber)?.uint64Value ?? 0
    }

    private func recordAppPresentationReceipt(
        presentationID: String,
        revision: UInt64
    ) {
        let key = "\(paneLayoutController.scopeID)|\(presentationID)"
        var receipts = AppDefaults.shared.dictionary(
            forKey: Self.appPresentationReceiptsKey
        ) ?? [:]
        let previous = (receipts[key] as? NSNumber)?.uint64Value ?? 0
        guard revision > previous else { return }
        receipts[key] = NSNumber(value: revision)
        AppDefaults.shared.set(receipts, forKey: Self.appPresentationReceiptsKey)
    }

    /// Project Host semantic `target: panel` bindings into this window's
    /// Controller-owned split tree. The Host never chooses a pane id, ratio,
    /// or width; the desktop convention is simply a trailing/right split on
    /// first reveal. Existing user placement is respected on later reveals.
    private func reconcileAppPresentations(_ envelope: AppPresentationsFile?) {
        guard selectedHostScope == .local,
              let envelope,
              envelope.version == 1
        else { return }

        let instances = Dictionary(
            envelope.instances.map { ($0.id, $0) },
            uniquingKeysWith: { first, _ in first }
        )
        for presentation in envelope.presentations
        where presentation.target == "panel" && presentation.revealRevision > 0 {
            guard presentation.revealRevision > appPresentationReceipt(
                presentationID: presentation.id
            ),
            let instance = instances[presentation.instanceID],
            let caller = sessionsByID[presentation.callerSessionID],
            let companion = sessionsByID[instance.companionSessionID],
            caller.isAttachable,
            companion.isAttachable,
            !archivedSessionIDs.contains(caller.id),
            !archivedSessionIDs.contains(companion.id)
            else { continue }

            let location: PaneLocation?
            if let existing = paneLayoutState.location(ofSession: companion.id) {
                // A Controller/user may already have placed the App pane. A
                // fresh reveal must surface it, never force it back to a new
                // geometry merely because the semantic request says panel.
                location = existing
            } else {
                location = mutatePaneLayout { state in
                    try state.insertSession(companion.id, beside: caller.id, at: .right)
                }
            }
            guard let location else { continue }

            // This one-shot only animates a mounted group. Background agents
            // still gain durable membership without stealing the user's
            // current selection; the panel appears when that group is shown.
            if selectedSessionID == caller.id
                || paneLayoutState.group(containingSession: selectedSessionID ?? "")?.id
                    == location.groupID {
                paneLayoutController.setPendingReveal(
                    groupID: location.groupID,
                    paneID: location.paneID
                )
            }
            recordAppPresentationReceipt(
                presentationID: presentation.id,
                revision: presentation.revealRevision
            )
        }
    }

    private func reconcilePaneLayout(eligibleSessionIDs: Set<String>) {
        paneLayoutController.reloadFromDisk()
        guard !paneLayoutState.groups.isEmpty else { return }
        let selectedGroupID = selectedSessionID.flatMap {
            paneLayoutState.location(ofSession: $0)?.groupID
        }
        let selectedWasRepresentative = selectedGroupID.flatMap { groupID in
            paneLayoutState.groups.first(where: { $0.id == groupID })
        }?.representativeSessionID == selectedSessionID

        let changes = paneLayoutController.mutate { state in
            state.reconcile(eligibleSessionIDs: eligibleSessionIDs)
        }
        guard !changes.isEmpty else { return }

        if let selectedGroupID,
           selectedWasRepresentative,
           let group = paneLayoutState.groups.first(where: {
               $0.id == selectedGroupID
           }),
           let promoted = group.representativeSessionID,
           promoted != selectedSessionID {
            selectedSessionID = promoted
        }

        if let reveal = paneLayoutController.pendingReveal,
           paneLayoutState.location(ofPane: reveal.paneID) == nil {
            paneLayoutController.clearPendingReveal()
        }
        if let focus = paneLayoutController.focusRequest,
           paneLayoutState.location(ofPane: focus.paneID) == nil {
            paneLayoutController.clearFocusRequest()
        }
        if let active = paneLayoutController.activePane,
           paneLayoutState.location(ofPane: active.paneID)?.groupID != active.groupID {
            clearActiveTerminalPane()
        }
        if let zoomed = paneLayoutController.zoomedPane,
           paneLayoutState.location(ofPane: zoomed.paneID)?.groupID != zoomed.groupID {
            paneLayoutController.clearZoom()
        }
        invalidateSidebarLists()
    }

    /// Render-time projection of one group. Dead/restarting Session panes are
    /// filtered without mutating state; the next reconciliation makes the
    /// repair durable. A launcher plus one Session remains valid transiently.
    func validatedPaneGroup(containingSession sessionID: String) -> PaneGroup? {
        guard let group = paneLayoutState.group(containingSession: sessionID)
        else { return nil }

        var root: PaneNode? = group.root
        for pane in group.root.leaves {
            switch pane.content {
            case .launcher:
                continue
            case let .session(id):
                let alive = displaySessionsByID[id].map { entry in
                    !restartingSessionIDs.contains(id)
                        && (entry.isAttachable || entry.status == .starting)
                } ?? false
                if !alive {
                    root = root?.removingLeaf(pane.id)
                }
            }
        }
        guard let root else { return nil }
        let sessionLeaves = root.sessionLeaves
        guard sessionLeaves.count >= 2
                || (sessionLeaves.count == 1 && root.containsLauncher)
        else { return nil }

        let representativePaneID = root.leaf(
            id: group.representativePaneID
        )?.content.sessionID != nil
            ? group.representativePaneID
            : sessionLeaves[0].id
        return PaneGroup(
            id: group.id,
            representativePaneID: representativePaneID,
            root: root
        )
    }

    func isPaneGroupRepresentative(_ sessionID: String) -> Bool {
        validatedPaneGroup(containingSession: sessionID)?
            .representativeSessionID == sessionID
    }

    /// Runtime marks represented by one collapsed sidebar row, in visual
    /// left-to-right pane order. Launcher slots contribute no fake mark.
    func paneSidebarItems(
        forRepresentative sessionID: String
    ) -> [PaneSidebarItem] {
        guard let group = validatedPaneGroup(containingSession: sessionID),
              group.representativeSessionID == sessionID
        else { return [] }

        return group.panes.compactMap { pane in
            guard let id = pane.content.sessionID,
                  let entry = displaySessionsByID[id]
            else { return nil }
            return Self.paneSidebarItem(
                paneID: pane.id,
                session: entry,
                isRepresentative: pane.id == group.representativePaneID
            )
        }
    }

    /// Select the group's representative row and focus the requested pane.
    func requestTerminalPaneFocus(
        representativeSessionID: String,
        sessionID: String
    ) {
        // The click itself supersedes any older exact-pane intent, even if
        // this tile raced a reconciliation and is no longer valid below.
        paneLayoutController.clearFocusRequest()
        guard let group = validatedPaneGroup(
            containingSession: representativeSessionID
        ),
        group.representativeSessionID == representativeSessionID,
        let pane = group.panes.first(where: {
            $0.content.sessionID == sessionID
        })
        else { return }

        selectedSessionID = representativeSessionID
        paneLayoutController.requestFocus(
            groupID: group.id,
            paneID: pane.id,
            sessionID: sessionID
        )
    }

    func paneGroupRepresentativeID(containing sessionID: String) -> String? {
        paneLayoutState.group(containingSession: sessionID)?
            .representativeSessionID
    }

    func swapTerminalPanes(
        groupID: String,
        _ paneID: String,
        with otherPaneID: String
    ) {
        guard paneLayoutState.location(ofPane: paneID)?.groupID == groupID,
              paneLayoutState.location(ofPane: otherPaneID)?.groupID == groupID
        else { return }
        _ = mutatePaneLayout { state in
            try state.swapPanes(paneID, with: otherPaneID)
        }
    }

    func resizePaneSplit(
        groupID: String,
        path: PaneSplitPath,
        ratio: Double
    ) {
        _ = mutatePaneLayout { state in
            try state.resizeSplit(in: groupID, at: path, ratio: ratio)
        }
    }

    func equalizeTerminalPanes(groupID: String) {
        _ = mutatePaneLayout { state in
            try state.equalize(groupID: groupID)
        }
    }

    /// Temporarily maximize the active pane (or the shown session's pane).
    /// Process-local presentation; the layout file is untouched.
    func toggleTerminalPaneZoom() {
        let target = paneLayoutController.activePane.map {
            (groupID: $0.groupID, paneID: $0.paneID)
        } ?? selectedSessionID.flatMap { shownID in
            paneLayoutState.location(ofSession: shownID).flatMap { location in
                validatedPaneGroup(containingSession: shownID) != nil
                    ? (groupID: location.groupID, paneID: location.paneID)
                    : nil
            }
        }
        guard let target,
              paneLayoutState.location(ofPane: target.paneID)?.groupID
                  == target.groupID
        else { return }
        paneLayoutController.toggleZoom(
            groupID: target.groupID,
            paneID: target.paneID
        )
    }

    var canZoomTerminalPane: Bool {
        guard let shownID = selectedSessionID else { return false }
        return validatedPaneGroup(containingSession: shownID) != nil
    }

    /// Whether ⌘W currently belongs to a mounted terminal pane rather than
    /// the AppKit window. This mirrors ContentArea's terminal presentation
    /// gate so settings, libraries, launchers, dead Sessions, and restart
    /// transitions keep the ordinary Close Window behavior.
    var canCloseActiveTerminalPane: Bool {
        guard !settingsVisible,
              archivedProjectID == nil,
              !recentActivityVisible,
              let shownID = selectedSessionID,
              let shown = displaySessionsByID[shownID]
        else { return false }

        let representative = validatedPaneGroup(
            containingSession: shownID
        )?.representativeSessionID.flatMap { displaySessionsByID[$0] } ?? shown
        return representative.status != .exited
            && !restartingSessionIDs.contains(representative.id)
    }

    func equalizeActiveTerminalPanes() {
        guard let shownID = selectedSessionID,
              let group = validatedPaneGroup(containingSession: shownID)
        else { return }
        equalizeTerminalPanes(groupID: group.id)
    }

    /// Move keyboard focus to the spatial neighbor of the active pane.
    func focusTerminalPane(_ direction: PaneEdge) {
        guard let shownID = selectedSessionID,
              let group = validatedPaneGroup(containingSession: shownID)
        else { return }
        let fromPaneID = paneLayoutController.activePane?.paneID
            ?? paneLayoutState.location(ofSession: shownID)?.paneID
        guard let fromPaneID,
              let neighbor = group.root.spatialNeighbor(
                  of: fromPaneID, direction: direction
              )
        else { return }
        // Zoom follows focus away: navigating out of a zoomed pane un-zooms.
        paneLayoutController.clearZoom()
        setActiveTerminalPane(
            groupID: group.id,
            paneID: neighbor.id,
            sessionID: neighbor.content.sessionID
        )
        if let sessionID = neighbor.content.sessionID {
            paneLayoutController.requestFocus(
                groupID: group.id,
                paneID: neighbor.id,
                sessionID: sessionID
            )
        }
    }

    /// Detaching changes only the window's layout. Sidebar organization stays
    /// exactly where the Host already stores it.
    func detachTerminalPane(_ paneID: String) {
        guard let groupID = paneLayoutState.location(ofPane: paneID)?.groupID,
              let oldGroup = paneLayoutState.groups.first(where: {
                  $0.id == groupID
              })
        else { return }
        let oldRepresentative = oldGroup.representativeSessionID
        guard mutatePaneLayout({ state in
            try state.detachPane(paneID)
        }) != nil else { return }

        if paneLayoutController.activePane?.paneID == paneID {
            clearActiveTerminalPane()
        }

        if selectedSessionID == oldRepresentative,
           let updated = paneLayoutState.groups.first(where: {
               $0.id == groupID
           }),
           let promoted = updated.representativeSessionID {
            selectedSessionID = promoted
        }
    }

    func closeTerminalPaneGroup(_ groupID: String) {
        _ = mutatePaneLayout { state in
            try state.closeGroup(groupID)
        }
        if paneLayoutController.activePane?.groupID == groupID {
            clearActiveTerminalPane()
        }
        paneLayoutController.clearFocusRequest()
        paneLayoutController.clearPendingReveal()
    }

    func canMoveSession(_ sessionID: String, toProjectID targetID: String) -> Bool {
        let routesThroughHost = routesSessionVerbThroughHost(sessionID)
        if routesThroughHost,
           !remoteHostRuntime.supportsHostOperation(
               RemoteHostRuntime.HostOperation.projectSet
           ) {
            return false
        }
        guard let session = displaySessionsByID[sessionID] else { return false }
        // The checkout rule (SessionMoveRules): a Session may be filed only
        // at its home — its worktree, or its root project — or in a plain
        // group directly under that home. A worktree Session therefore never
        // leaves its worktree; the shell keeps running there regardless.
        return SessionMoveRules.canFile(
            sessionProjectID: session.projectID,
            effectiveProjectID: displayEffectiveProjectID(for: session),
            targetID: targetID,
            projectsByID: displayProjectsByID
        )
    }

    /// A drag over a row owned by `hoveringProjectID` that would cross a git
    /// checkout boundary: the drop is refused with the "no" shake and no
    /// target ever highlights.
    func sessionMoveCrossesCheckout(_ sessionID: String, hoveringProjectID: String) -> Bool {
        guard let session = displaySessionsByID[sessionID] else { return false }
        return SessionMoveRules.crossesCheckout(
            sessionProjectID: session.projectID,
            hoveredProjectID: hoveringProjectID,
            projectsByID: displayProjectsByID
        )
    }

    /// File a session under a plain organizational group, or back at its root
    /// project. Git worktrees are deliberately rejected because changing a
    /// checkout needs restart/resume, not this display-only override.
    /// Cross-frontend via the shared `project-override.json` marker.
    func moveSession(_ sessionID: String, toProjectID targetID: String) {
        // Scoped/remote rows ride the protocol move verb; the Host writes its
        // own override marker and the refreshed bootstrap reconciles.
        if routesSessionVerbThroughHost(sessionID) {
            performRemoteVerb("Couldn't move the session") { [weak self] runtime in
                guard let self else { return }
                if self.remoteProjectSummariesByID[targetID] == nil {
                    runtime.requestImmediateRefresh()
                    for _ in 0..<30
                    where self.remoteProjectSummariesByID[targetID] == nil {
                        try await Task.sleep(nanoseconds: 100_000_000)
                    }
                }
                guard self.remoteProjectSummariesByID[targetID] != nil else {
                    throw RemoteHostVerbError(
                        operation: "move session",
                        message: "The Host has not published the destination project yet.",
                        outcomeIsUnknown: false
                    )
                }
                try await runtime.setSessionProject(sessionID, projectID: targetID)
            }
            return
        }
        guard canMoveSession(sessionID, toProjectID: targetID),
              let session = sessionsByID[sessionID]
        else { return }
        let sourceProjectID = effectiveProjectID(for: session)
        let wasPinned = isPinned(sessionID: sessionID, projectID: sourceProjectID)
        if targetID == session.projectID {
            Self.removeSharedMarker(sessionID, .projectOverride)
        } else {
            Self.writeSharedMarker(sessionID, .projectOverride, [
                "project_id": targetID,
                "moved_at": Int64(Date().timeIntervalSince1970 * 1000),
            ])
            expandedProjectIDs.insert(targetID)
        }
        announceStateChange("session-markers")
        rescan()
        // Pins are project-scoped. Preserve that intent when filing a pinned
        // session by moving its pin record to the destination after the
        // synchronous rescan has adopted the new override.
        if wasPinned {
            pinSession(projectID: targetID, sessionID: sessionID)
        }
    }

    /// Destinations for a session's display-only "Move to" menu: its root
    /// project plus plain organizational groups. Git worktrees stay out of
    /// this menu because entering one requires an explicit restart/resume.
    func moveDestinations(forSession sessionID: String) -> [(id: String, name: String)] {
        // Scoped/remote rows ride the `session.project.set` verb; the menu
        // hides only when the Host does not advertise it.
        if routesSessionVerbThroughHost(sessionID),
           !remoteHostRuntime.supportsHostOperation(
               RemoteHostRuntime.HostOperation.projectSet
           ) {
            return []
        }
        guard let session = displaySessionsByID[sessionID] else { return [] }
        // Same checkout rule the drag enforces (SessionMoveRules): the
        // Session's home — its worktree or root — plus that home's plain
        // groups, so the menu never offers what the drag refuses. The
        // panel's storage group never surfaces as a group: Pin to project
        // sidebar is its only entry.
        return SessionMoveRules.destinations(
            sessionProjectID: session.projectID,
            effectiveProjectID: displayEffectiveProjectID(for: session),
            projectsByID: displayProjectsByID,
            isHiddenGroup: { isProjectSidebarGroup($0) }
        ).map { ($0.id, $0.name) }
    }

    // MARK: - Project sidebar (right-side session stack)

    /// THE seam for against-the-home shared-state writes: this instance's own
    /// home for Local, the scoped workspace's home for a local workspace (the
    /// same write class as Add Project — files ON this machine), nil for a
    /// true remote Host, whose state is only reachable through protocol
    /// verbs. New features should route file writes through this instead of
    /// gating on `selectedHostScope == .local`, so local workspaces inherit
    /// them automatically.
    var scopedStateHome: URL? {
        switch selectedHostScope {
        case .local:
            return LaunchConfig.appSessionsDir.deletingLastPathComponent()
        case .localWorkspace(let home, _):
            return URL(fileURLWithPath: home)
        case .remote:
            return nil
        }
    }

    /// `app-sessions` directory whose `session.sock`/`output.bin` the
    /// terminal surfaces of the current scope attach to. Local workspaces
    /// share the direct `unpeel-attach` data plane with Local scope; a true
    /// remote Host has no local directory and never reaches this path.
    var scopedSessionsDir: URL {
        switch selectedHostScope {
        case .local, .remote:
            return LaunchConfig.appSessionsDir
        case .localWorkspace(let home, _):
            return URL(fileURLWithPath: home, isDirectory: true)
                .appendingPathComponent("app-sessions", isDirectory: true)
        }
    }

    /// Effective (display) project for a row in the CURRENT scope: the local
    /// override resolution for Local; scoped rows already carry the Host's
    /// resolved placement in `projectID`.
    func displayEffectiveProjectID(for session: SessionEntry) -> String {
        selectedHostScope == .local
            ? effectiveProjectID(for: session)
            : session.projectID
    }

    /// The per-root-project "Sidebar" group id is deterministic so the feature
    /// survives renames: the group itself is an ordinary plain group (shared
    /// state, pinned at top), so the TUI and phone see it like any other group
    /// — the right panel is just the desktop's presentation of its members.
    static func projectSidebarGroupID(forRoot rootID: String) -> String {
        "sidebar-" + rootID
    }

    /// Root project (top of the parent chain) in the current scope's tree.
    private func rootProjectID(forProjectID projectID: String) -> String {
        var id = projectID
        var hops = 0
        while let parent = displayProjectsByID[id]?.parentProjectID, hops < 16 {
            id = parent
            hops += 1
        }
        return id
    }

    /// The root project the window is currently "in": the selected session's
    /// root, falling back to the first project in the tree.
    var currentRootProjectID: String? {
        if let id = sessionSelection.sessionID,
           let session = displaySessionsByID[id] {
            return rootProjectID(forProjectID: displayEffectiveProjectID(for: session))
        }
        return displayNodes.first.map { rootProjectID(forProjectID: $0.project.id) }
    }

    /// Selection lives in its own observable so hot session switches don't
    /// re-render the whole window. The right panel's visibility DOES depend on
    /// which project the selection is in, so this mirrors just the root
    /// project as store state — published only when it actually changes
    /// (cross-project moves), keeping RootView out of the hot path.
    @Published private(set) var activeRootProjectID: String?

    private func refreshActiveRootProject(selectedID: String?) {
        let next: String?
        if let selectedID,
           let session = displaySessionsByID[selectedID] {
            next = rootProjectID(forProjectID: displayEffectiveProjectID(for: session))
        } else {
            next = displayNodes.first.map { rootProjectID(forProjectID: $0.project.id) }
        }
        if next != activeRootProjectID {
            activeRootProjectID = next
            // The panel's transient launcher belongs to the project it was
            // opened in; a project switch dismisses it.
            projectSidebarLauncher = nil
        }
    }

    /// Live sessions filed in the CURRENT root project's sidebar group, in the
    /// sidebar tree's display order. Non-empty ⇒ the right panel shows. The
    /// projection is scope-neutral: local and scoped trees resolve the same
    /// way (a scoped workspace's group travels in its bootstrap).
    var projectSidebarSessions: [SessionEntry] {
        guard let rootID = currentRootProjectID else { return [] }
        let groupID = Self.projectSidebarGroupID(forRoot: rootID)
        guard displayProjectsByID[groupID] != nil else { return [] }
        func find(_ nodes: [ProjectNode]) -> ProjectNode? {
            for node in nodes {
                if node.project.id == groupID { return node }
                if let hit = find(node.worktrees) { return hit }
            }
            return nil
        }
        guard let node = find(displayNodes) else { return [] }
        return node.sessions.filter(\.isLive)
    }

    /// True when this project id is a root project's "Sidebar" group.
    /// Resolves through the scoped map first, then local truth: the swipe
    /// carousel's ghost of the Local workspace renders `nodes` while another
    /// scope is selected, and a display-map-only lookup missed the local
    /// group there — a stray "Sidebar" folder appeared only mid-swipe.
    func isProjectSidebarGroup(_ projectID: String) -> Bool {
        guard let project = displayProjectsByID[projectID] ?? projectsByID[projectID],
              project.isFolder == true,
              let parent = project.parentProjectID
        else { return false }
        return projectID == Self.projectSidebarGroupID(forRoot: parent)
    }

    /// True when the session is filed in ANY project's sidebar group. Such
    /// sessions render in the right panel; the main area must not also mount
    /// them (one terminal surface cannot live in two containers).
    func sessionIsInProjectSidebar(_ sessionID: String) -> Bool {
        guard let session = displaySessionsByID[sessionID] else { return false }
        return isProjectSidebarGroup(displayEffectiveProjectID(for: session))
    }

    /// Pin/unpin uses `session.project.set` for Host-controlled scopes. The
    /// old direct Local path writes the equivalent override marker.
    func canMoveSessionToProjectSidebar(_ sessionID: String) -> Bool {
        guard let session = displaySessionsByID[sessionID],
              session.isLive,
              !sessionIsInProjectSidebar(sessionID),
              // The project sidebar is the ROOT project's storage group. A
              // worktree Session may not be filed outside its worktree
              // (SessionMoveRules), so it never pins there.
              !SessionMoveRules.isWorktreeBound(
                  sessionProjectID: session.projectID,
                  projectsByID: displayProjectsByID
              )
        else { return false }
        if selectedHostScope == .local, !localHostClientStarted { return true }
        if selectedHostScope.isLocalMachine {
            return remoteHostRuntime.supportsHostOperation(
                RemoteHostRuntime.HostOperation.projectSet
            )
        }
        // TRUE remote Hosts: the move verb must be advertised, and the
        // sidebar group must already exist there (remote group creation is
        // still unbuilt — the Host's own desktop/TUI mints it).
        guard remoteHostRuntime.supportsHostOperation(
            RemoteHostRuntime.HostOperation.projectSet
        ) else { return false }
        let rootID = rootProjectID(forProjectID: displayEffectiveProjectID(for: session))
        return displayProjectsByID[Self.projectSidebarGroupID(forRoot: rootID)] != nil
    }

    /// "Pin to project sidebar": file the session into its root project's
    /// sidebar group (created on first use, pinned to the top of the project).
    /// Group creation is still a locked shared-state write on this machine;
    /// filing the Session itself is the ordinary Host project-set verb once
    /// the worker publishes that group. A session in a main-area split
    /// detaches first.
    func moveSessionToProjectSidebar(_ sessionID: String) {
        guard canMoveSessionToProjectSidebar(sessionID),
              let session = displaySessionsByID[sessionID]
        else { return }
        let rootID = rootProjectID(forProjectID: displayEffectiveProjectID(for: session))
        if let location = paneLayoutState.location(ofSession: sessionID) {
            detachTerminalPane(location.paneID)
        }
        if selectedHostScope == .local {
            guard let groupID = ensureProjectSidebarGroup(rootID: rootID) else { return }
            moveSession(sessionID, toProjectID: groupID)
        } else if let home = scopedStateHome {
            guard let groupID = ensureScopedProjectSidebarGroup(
                rootID: rootID, home: home
            ) else { return }
            moveSession(sessionID, toProjectID: groupID)
        } else {
            // TRUE remote: protocol move into the Host's existing group
            // (the can-gate above proved both verb and group).
            let groupID = Self.projectSidebarGroupID(forRoot: rootID)
            guard displayProjectsByID[groupID] != nil else { return }
            moveSession(sessionID, toProjectID: groupID)
        }
    }

    /// "Unpin": un-file a sidebar-group member back to its own project, so it
    /// leaves the right panel and renders like any ordinary session again.
    /// Sessions moved in carry an override marker (dropped here); sessions
    /// LAUNCHED in the panel under the group's manifest move to its parent.
    func moveSessionToMainArea(_ sessionID: String) {
        guard sessionIsInProjectSidebar(sessionID),
              let session = displaySessionsByID[sessionID]
        else { return }
        if currentScopeUsesHostControl {
            let effective = displayEffectiveProjectID(for: session)
            guard let parent = displayProjectsByID[effective]?.parentProjectID
            else { return }
            moveSession(sessionID, toProjectID: parent)
        } else if selectedHostScope == .local {
            let effective = effectiveProjectID(for: session)
            if session.projectID == effective,
               let parent = projectsByID[effective]?.parentProjectID {
                moveSession(sessionID, toProjectID: parent)
            } else {
                moveSession(sessionID, toProjectID: session.projectID)
            }
        } else if let home = scopedStateHome {
            // Scoped pins always travel as override markers; dropping the
            // marker restores the manifest project. (Scoped panel launches
            // create in the root + marker, never a group manifest.)
            Self.removeSharedMarker(sessionID, .projectOverride, home: home)
        } else {
            // TRUE remote: move back to the group's parent over the verb.
            let effective = displayEffectiveProjectID(for: session)
            guard let parent = displayProjectsByID[effective]?.parentProjectID
            else { return }
            moveSession(sessionID, toProjectID: parent)
        }
    }

    /// Find-or-create the sidebar group in a SCOPED local workspace's own
    /// app-state.json (locked shared write against its home; its host picks
    /// the change up from disk and pushes a fresh bootstrap). No native
    /// record: `native-` ids are each instance's own mirror namespace.
    private func ensureScopedProjectSidebarGroup(
        rootID: String, home: URL
    ) -> String? {
        let groupID = Self.projectSidebarGroupID(forRoot: rootID)
        if displayProjectsByID[groupID] != nil { return groupID }
        guard let root = displayProjectsByID[rootID] else { return nil }
        let stateURL = home.appendingPathComponent("app-state.json")
        let wrote = PresetStateFile.edit(at: stateURL) { object in
            var projects = (object["projects"] as? [[String: Any]]) ?? []
            guard !projects.contains(where: { ($0["id"] as? String) == groupID })
            else { return }
            projects.append([
                "id": groupID,
                "name": "Sidebar",
                "path": root.path,
                "parent_project_id": rootID,
                "is_folder": true,
                "pinned_at": Int64(Date().timeIntervalSince1970 * 1000),
            ])
            object["projects"] = projects
        }
        return wrote ? groupID : nil
    }

    /// Transient "add a pane" launcher row in the right panel, POSITIONED
    /// where the new pane will arrive — directly below its anchor member —
    /// matching the main area's launcher-pane-at-destination behavior.
    /// `afterSessionID == nil` appends at the stack's end. Never persisted.
    struct ProjectSidebarLauncher: Equatable {
        let afterSessionID: String?
        /// Set once a preset was chosen: the launcher slot stays mounted
        /// (spinner) until this session joins the stack and replaces its
        /// content IN PLACE — the pane never jumps.
        var pendingSessionID: String? = nil
    }

    /// Called by the panel when the pending session has landed in the stack:
    /// the launcher slot's job is done, the pane occupies its frame.
    func clearProjectSidebarLauncherIfLanded() {
        guard let pending = projectSidebarLauncher?.pendingSessionID,
              projectSidebarSessions.contains(where: { $0.id == pending })
        else { return }
        projectSidebarLauncher = nil
    }

    @Published var projectSidebarLauncher: ProjectSidebarLauncher?

    /// Compatibility spelling used by view code that only needs presence.
    var projectSidebarLauncherVisible: Bool { projectSidebarLauncher != nil }

    /// Whether adding a pane to the panel is possible in the current scope:
    /// the pin itself needs a home on this machine (the override marker), and
    /// a scoped workspace additionally needs the Host's create operation.
    var projectSidebarLauncherAvailable: Bool {
        guard scopedStateHome != nil else { return false }
        if selectedHostScope == .local, !localHostClientStarted { return true }
        return remoteHostRuntime.supportsHostOperation(
            RemoteHostRuntime.HostOperation.create
        )
    }

    /// ⌘D routes here instead of the main pane launcher while the selection
    /// is a panel member — the main launcher would otherwise pull the panel
    /// session into the main pane layout.
    func canOpenProjectSidebarLauncher() -> Bool {
        guard projectSidebarLauncherAvailable,
              let id = selectedSessionID
        else { return false }
        return sessionIsInProjectSidebar(id)
    }

    func openProjectSidebarLauncher() {
        guard canOpenProjectSidebarLauncher() else { return }
        // ⌘D anchors below the selected member — the pane the user is "in".
        projectSidebarLauncher = ProjectSidebarLauncher(
            afterSessionID: selectedSessionID
        )
    }

    /// The panel pane-header split button: unlike the ⌘D route this never
    /// depends on the global selection — the button lives on a panel pane, so
    /// the launcher opens right below THAT pane.
    func openProjectSidebarLauncherFromPanel(afterSessionID: String?) {
        guard projectSidebarLauncherAvailable else { return }
        projectSidebarLauncher = ProjectSidebarLauncher(
            afterSessionID: afterSessionID
        )
    }

    /// Launch straight into the sidebar group. The legacy Local path spawns
    /// with the group as its manifest project. Host-controlled scopes create
    /// in the root (the create verb refuses folder targets), then file and
    /// order the row through Host verbs. Both stay in the background — adding
    /// a panel pane must never steal the user's current selection; the pane
    /// appearing IS the feedback.
    func launchInProjectSidebar(preset: Preset) {
        guard projectSidebarLauncher?.pendingSessionID == nil else { return }
        let anchorID = projectSidebarLauncher?.afterSessionID
        guard let rootID = currentRootProjectID else {
            projectSidebarLauncher = nil
            return
        }

        // The new pane lands where its launcher row sat: directly below the
        // anchor member (append when there is none). Unknown ids are fine in
        // the order list — it applies as rows appear.
        func orderedIDs(inserting sessionID: String) -> [String] {
            var ids = projectSidebarSessions.map(\.id).filter { $0 != sessionID }
            let index = anchorID.flatMap { ids.firstIndex(of: $0) }
                .map { $0 + 1 } ?? ids.count
            ids.insert(sessionID, at: min(index, ids.count))
            return ids
        }

        guard let home = scopedStateHome,
              let groupID = ensureScopedProjectSidebarGroup(rootID: rootID, home: home)
        else {
            projectSidebarLauncher = nil
            return
        }
        let presetID = preset.command.isEmpty ? nil : preset.id
        performRemoteVerb("Couldn't start the session", onFailure: { [weak self] in
            self?.projectSidebarLauncher = nil
        }) { [weak self] runtime in
            guard let self else { return }
            runtime.requestImmediateRefresh()
            for _ in 0..<30 where self.remoteProjectSummariesByID[groupID] == nil {
                try await Task.sleep(nanoseconds: 100_000_000)
            }
            guard self.remoteProjectSummariesByID[groupID] != nil else {
                throw RemoteHostVerbError(
                    operation: "new sidebar Session",
                    message: "The Host has not published the Sidebar group yet.",
                    outcomeIsUnknown: false
                )
            }
            let sessionID = try await runtime.createSession(
                projectID: rootID,
                presetID: presetID,
                command: presetID == nil ? preset.command : nil,
                selectOnCreate: false
            )
            self.projectSidebarLauncher = ProjectSidebarLauncher(
                afterSessionID: anchorID, pendingSessionID: sessionID
            )
            try await runtime.setSessionProject(sessionID, projectID: groupID)
            try await runtime.setSessionOrder(
                projectID: groupID,
                orderedSessionIDs: orderedIDs(inserting: sessionID)
            )
        }
    }

    /// Drag-drop pin: file the session into the sidebar group AND land at the
    /// dropped stack position. Locally the rescan inside the move is
    /// synchronous, so the order write applies immediately; a scoped drop
    /// sends the intended order over the `order.set` verb (the workspace
    /// applies it as its bootstrap lists the row).
    func pinSessionToProjectSidebar(_ sessionID: String, at index: Int?) {
        let members = projectSidebarSessions.map(\.id)
        moveSessionToProjectSidebar(sessionID)
        guard let index else { return }
        var ids = members.filter { $0 != sessionID }
        ids.insert(sessionID, at: min(max(index, 0), ids.count))
        if currentScopeUsesHostControl, let rootID = currentRootProjectID {
            let groupID = Self.projectSidebarGroupID(forRoot: rootID)
            setSessionOrder(projectID: groupID, ids: ids)
        } else if selectedHostScope == .local {
            guard let session = sessionsByID[sessionID] else { return }
            let groupID = effectiveProjectID(for: session)
            guard isProjectSidebarGroup(groupID) else { return }
            setSessionOrder(projectID: groupID, ids: ids)
        }
    }

    /// Panel stack reorder commit for the current scope: Local writes the
    /// shared order directly; a scoped workspace/Host rides the `order.set`
    /// protocol verb over the group.
    func commitProjectSidebarReorder(
        draggedID: String, over targetID: String, groupID: String
    ) {
        var ids = projectSidebarSessions.map(\.id)
        guard let from = ids.firstIndex(of: draggedID),
              let to = ids.firstIndex(of: targetID)
        else { return }
        ids.remove(at: from)
        ids.insert(draggedID, at: to)
        setSessionOrder(projectID: groupID, ids: ids)
    }

    /// Find-or-create the root project's sidebar group: a plain native-record
    /// child group with the deterministic id, mirrored into shared state and
    /// pinned so it renders at the top of the project.
    private func ensureProjectSidebarGroup(rootID: String) -> String? {
        let groupID = Self.projectSidebarGroupID(forRoot: rootID)
        if projectsByID[groupID] != nil { return groupID }
        guard let root = projectsByID[rootID] else { return nil }
        var records = loadNativeProjects()
        guard !records.contains(where: { $0.id == groupID }) else { return groupID }
        records.append(NativeProjectRecord(
            id: groupID,
            name: "Sidebar",
            path: root.path,
            parentProjectID: rootID,
            isFolder: true
        ))
        guard let data = try? JSONEncoder().encode(records) else { return nil }
        AppDefaults.shared.set(data, forKey: Self.nativeProjectsKey)
        mirrorProjectsToSharedState()
        expandedProjectIDs.insert(rootID)
        rescan()
        setGroupPinned(groupID, pinned: true)
        return groupID
    }

    /// "Remove worktree" on a worktree child project: native confirm
    /// dialog, `git worktree remove` (refuses while dirty), then forget the
    /// project. A dirty refusal comes back as a second, destructive
    /// "Force Delete" confirmation that retries with --force. Committed
    /// work stays on the branch either way
    /// (handleRemoveProject, Sidebar.svelte:74-106).
    func removeWorktreeProject(_ projectID: String) {
        guard let project = projectsByID[projectID],
              let branch = project.worktreeBranch
        else { return }
        let parentPath = project.parentProjectID.flatMap { projectsByID[$0]?.path }

        let alert = NSAlert()
        alert.messageText = "Remove worktree \"\(project.name)\"?"
        alert.informativeText = """
        This deletes the worktree folder from disk. Committed work stays on \
        the "\(branch)" branch, and git will refuse if there are unsaved \
        changes.
        """
        alert.alertStyle = .warning
        alert.addButton(withTitle: "Remove Worktree")
        alert.addButton(withTitle: "Cancel")
        guard alert.runModal() == .alertFirstButtonReturn else { return }

        runWorktreeRemoval(
            projectID: projectID, projectName: project.name, branch: branch,
            parentPath: parentPath, worktreePath: project.path, force: false
        )
    }

    private func runWorktreeRemoval(
        projectID: String, projectName: String, branch: String,
        parentPath: String?, worktreePath: String, force: Bool
    ) {
        Task { [weak self] in
            let outcome = await Task.detached(priority: .userInitiated) {
                WorktreeGit.removeWorktree(
                    repoPath: parentPath, worktreePath: worktreePath, force: force
                )
            }.value
            await MainActor.run {
                guard let self else { return }
                switch outcome {
                case .removed, .alreadyGone:
                    self.removeProject(projectID)
                case .dirty:
                    guard Self.confirmForceRemoveWorktree(
                        name: projectName, branch: branch
                    ) else { return }
                    self.runWorktreeRemoval(
                        projectID: projectID, projectName: projectName, branch: branch,
                        parentPath: parentPath, worktreePath: worktreePath, force: true
                    )
                case .failed(let message):
                    Self.showErrorAlert(title: "Couldn't remove worktree", message: message)
                }
            }
        }
    }

    private static func confirmForceRemoveWorktree(name: String, branch: String) -> Bool {
        let alert = NSAlert()
        alert.messageText = "\"\(name)\" has unsaved changes"
        alert.informativeText = """
        The worktree contains modified or untracked files that are not \
        committed. Force deleting discards them permanently. Committed work \
        stays on the "\(branch)" branch.
        """
        alert.alertStyle = .critical
        let forceButton = alert.addButton(withTitle: "Force Delete")
        forceButton.hasDestructiveAction = true
        alert.addButton(withTitle: "Cancel")
        return alert.runModal() == .alertFirstButtonReturn
    }

    private static func showErrorAlert(title: String, message: String) {
        let alert = NSAlert()
        alert.messageText = title
        alert.informativeText = message
        alert.alertStyle = .warning
        alert.addButton(withTitle: "OK")
        alert.runModal()
    }

    // MARK: - Presets (flat ordered list; preset.rs list_presets)

    /// Native-side preset overrides persisted in UserDefaults. The native
    /// app is GLOBAL-presets-only. The Tauri app owns
    /// ~/.unpeel/app-state.json, so we never write it; native mutations
    /// are merged over the file presets at read time, native wins by id
    /// (same pattern as the pin overrides).
    struct PresetOverlay: Codable {
        var added: [Preset] = []
        var edited: [Preset] = []
        var removedIDs: [String] = []
    }

    /// Legacy on-disk shape: the overlay used to be keyed by scope
    /// ("global" or a project id). Only the "global" entry is migrated;
    /// project-scope entries are dropped (project presets were removed).
    private func loadPresetOverlay() -> PresetOverlay {
        guard let data = AppDefaults.shared.data(forKey: Self.nativePresetsKey)
        else { return PresetOverlay() }
        if let overlay = try? JSONDecoder().decode(PresetOverlay.self, from: data) {
            return overlay
        }
        if let legacy = try? JSONDecoder().decode([String: PresetOverlay].self, from: data) {
            return legacy["global"] ?? PresetOverlay()
        }
        return PresetOverlay()
    }

    private func savePresetOverlay(_ overlay: PresetOverlay) {
        if let data = try? JSONEncoder().encode(overlay) {
            AppDefaults.shared.set(data, forKey: Self.nativePresetsKey)
        }
    }

    /// Apply the overlay to the file-based presets:
    /// removedIDs hide file entries, edited replaces file entries by id,
    /// added appends. Everything is quick-launch-sanitized on the way out
    /// (sanitize_preset_quick_launch parity, like list_presets_for_project).
    private func overlaid(_ base: [Preset], overlay: PresetOverlay) -> [Preset] {
        let removed = Set(overlay.removedIDs)
        var editedByID: [String: Preset] = [:]
        for preset in overlay.edited { editedByID[preset.id] = preset }

        var result = base
            .filter { !removed.contains($0.id) }
            .map { editedByID[$0.id] ?? $0 }
        let baseIDs = Set(base.map(\.id))
        result.append(contentsOf: overlay.added.filter {
            !removed.contains($0.id) && !baseIDs.contains($0.id)
        })
        return result.map { $0.sanitized() }
    }

    /// Build the preset lists and quick strip (shared by every project).
    /// Migrated installs read app-state.json alone — the array order IS the
    /// display order, shared with the TUI. Un-migrated installs still layer
    /// the legacy UserDefaults overlay, then fold it into the file one-shot.
    private func rebuildPresets(
        globalPresets: [Preset],
        setupDone: Bool,
        overlayMigrated: Bool,
        allowFold: Bool
    ) {
        let loaded: [Preset]
        if overlayMigrated {
            presetsInSharedFile = true
            loaded = globalPresets.map { $0.sanitized() }
        } else {
            migrateCLIPreferencesIfNeeded(globalPresets: globalPresets, setupDone: setupDone)
            loaded = orderApplied(overlaid(globalPresets, overlay: loadPresetOverlay()))
            // A file that exists but did not decode must never be folded
            // over — `globalPresets` would be the builtin fallback, not the
            // user's list.
            presetsInSharedFile = allowFold && migrateOverlayPresetsToSharedFile(loaded)
        }
        // Presets are present-or-deleted in the native UI. Keep the encoded
        // `enabled` field for old clients and protocol compatibility, but do
        // not let an old false value strand a preset with no way to restore
        // it. PATH availability is likewise informational, not a launch gate.
        let merged = loaded.map { preset in
            var preset = preset
            preset.enabled = true
            return preset
        }
        let enabled = merged
        let available = merged
        // Starred presets grouped by CLI, in flat-list order; the strip shows
        // one chip per group (dropdown when a CLI has 2+ starred presets).
        let groups = collectQuickPresetGroups(available)
        let quick = groups.map(\.leader) + [.newTerminal]

        if merged != mergedPresets {
            mergedPresets = merged
        }
        if enabled != enabledPresets {
            enabledPresets = enabled
        }
        if available != availablePresets {
            availablePresets = available
        }
        if groups != quickPresetGroups {
            quickPresetGroups = groups
        }
        if quick != quickPresets {
            quickPresets = quick
        }
    }

    /// nil = the PATH scan hasn't completed yet.
    func isCLIInstalled(_ cli: SetupTool) -> Bool? {
        setupToolReport?.status(for: cli)?.installed
    }

    /// One-time fold of the UserDefaults preset overlay (adds/edits/removals
    /// plus the flat display order) into app-state.json, which from then on
    /// is the single preset truth both UIs read and write. The overlay keys
    /// are left in place — defaults are shared by bundle id, so an older
    /// build running side by side must keep its state — but this build never
    /// reads them again once the file carries the marker. On write failure
    /// the overlay stays authoritative and the fold retries next rescan.
    private func migrateOverlayPresetsToSharedFile(_ merged: [Preset]) -> Bool {
        editPresetStateAnnouncing { object in
            let existing = PresetStateFile.rawPresets(of: object)
            var byID: [String: [String: Any]] = [:]
            for dict in existing {
                if let id = dict["id"] as? String { byID[id] = dict }
            }
            let mergedIDs = Set(merged.map(\.id))
            var rewritten = merged.map { preset in
                PresetStateFile.apply(preset, to: byID[preset.id] ?? ["project_id": NSNull()])
            }
            // Keep rows this build does not model (Tauri-era project-scoped
            // presets) — only global rows the overlay removed are meant to
            // disappear.
            rewritten.append(contentsOf: existing.filter { dict in
                guard let id = dict["id"] as? String, !mergedIDs.contains(id) else {
                    return false
                }
                let projectID = dict["project_id"]
                return projectID != nil && !(projectID is NSNull)
            })
            object["presets"] = rewritten
            object[PresetStateFile.migratedKey] = true
        }
    }

    // MARK: - Flat preset order (native UserDefaults overlay)

    /// Sort presets by the saved flat order. Ids missing from the order (new
    /// presets, or an empty order) append at the end in their incoming
    /// (app-state.json) order.
    private func orderApplied(_ presets: [Preset]) -> [Preset] {
        guard !presetOrder.isEmpty else { return presets }
        let rank = Dictionary(
            uniqueKeysWithValues: presetOrder.enumerated().map { ($1, $0) }
        )
        return presets.enumerated()
            .sorted { lhs, rhs in
                let lhsRank = rank[lhs.element.id] ?? Int.max
                let rhsRank = rank[rhs.element.id] ?? Int.max
                if lhsRank != rhsRank { return lhsRank < rhsRank }
                return lhs.offset < rhs.offset
            }
            .map(\.element)
    }

    /// Reorder the preset list. `currentOrder` is the preset-id order the list
    /// was showing (so drag indices line up with what the user sees).
    /// `currentOrder` may be a visible subset; presets outside it keep their
    /// prior relative order, appended after the reordered visible ones.
    func movePresets(_ currentOrder: [String], from offsets: IndexSet, to destination: Int) {
        var order = currentOrder
        order.move(fromOffsets: offsets, toOffset: destination)
        let visible = Set(order)
        let rest = mergedPresets.map(\.id).filter { !visible.contains($0) }
        applyPresetOrder(order + rest)
        rescan()
    }

    /// Persist the visible order from a completed drag (one write per drop).
    func reorderPresets(_ visibleOrder: [String]) {
        let visible = Set(visibleOrder)
        let rest = mergedPresets.map(\.id).filter { !visible.contains($0) }
        applyPresetOrder(visibleOrder + rest)
        rescan()
    }

    /// Persist a full flat order. Migrated installs rewrite the file's
    /// presets array — the array order IS the display order everywhere,
    /// including the TUI; rows missing from `fullOrder` (project-scoped or
    /// unmodelled) keep their relative position at the end. Un-migrated
    /// installs keep the legacy presetOrder overlay.
    private func applyPresetOrder(_ fullOrder: [String]) {
        if presetsInSharedFile {
            let rank = Dictionary(
                uniqueKeysWithValues: fullOrder.enumerated().map { ($1, $0) }
            )
            editPresetStateAnnouncing { object in
                object["presets"] = PresetStateFile.rawPresets(of: object)
                    .enumerated()
                    .sorted { lhs, rhs in
                        let lhsRank = (lhs.element["id"] as? String)
                            .flatMap { rank[$0] } ?? Int.max
                        let rhsRank = (rhs.element["id"] as? String)
                            .flatMap { rank[$0] } ?? Int.max
                        if lhsRank != rhsRank { return lhsRank < rhsRank }
                        return lhs.offset < rhs.offset
                    }
                    .map(\.element)
            }
        } else {
            presetOrder = fullOrder
            savePresetOrder()
        }
    }

    private static func loadPresetOrder() -> [String] {
        guard let data = AppDefaults.shared.data(forKey: nativePresetOrderKey),
              let order = try? JSONDecoder().decode([String].self, from: data)
        else { return [] }
        return order
    }

    private func savePresetOrder() {
        if let data = try? JSONEncoder().encode(presetOrder) {
            AppDefaults.shared.set(data, forKey: Self.nativePresetOrderKey)
        }
    }

    /// One-time fold of the legacy per-CLI preferences (display order, hide
    /// toggles, MCP default choices) into the flat preset list:
    /// - the initial `presetOrder` reproduces the old derived order (CLI rank
    ///   over the saved CLI order, custom commands last),
    /// - each explicit per-CLI default preset is hoisted above its CLI
    ///   siblings (order-derived defaults keep the same MCP behavior),
    /// - the retired CLI-availability toggle is ignored; presets are now
    ///   present-or-deleted and PATH availability does not hide them.
    /// Runs once: guarded on the presetOrder key being absent. Fresh machines
    /// (no legacy keys, setup not completed) skip it entirely so first-run
    /// usage-based seeding can produce the first order instead.
    private func migrateCLIPreferencesIfNeeded(globalPresets: [Preset], setupDone: Bool) {
        guard AppDefaults.shared.object(forKey: Self.nativePresetOrderKey) == nil else { return }
        let defaults = AppDefaults.shared
        let hasLegacy = defaults.object(forKey: Self.legacyCLIOrderKey) != nil
            || defaults.object(forKey: Self.legacyCLIDefaultsKey) != nil
            || defaults.object(forKey: Self.legacyCLIAvailabilityKey) != nil
        guard hasLegacy || setupDone else { return }

        func decode<T: Decodable>(_ type: T.Type, key: String) -> T? {
            defaults.data(forKey: key).flatMap { try? JSONDecoder().decode(type, from: $0) }
        }
        let legacyOrder = decode([String].self, key: Self.legacyCLIOrderKey) ?? []
        let legacyDefaults = decode([String: String].self, key: Self.legacyCLIDefaultsKey) ?? [:]
        let overlay = loadPresetOverlay()
        let merged = overlaid(globalPresets, overlay: overlay)

        // Old derived order: saved CLI order first, then declaration order;
        // custom commands last, ties keep app-state order.
        let savedCLIs = legacyOrder.compactMap { SetupTool(rawValue: $0) }
        let savedCLISet = Set(savedCLIs)
        let orderedCLIs = savedCLIs + SetupTool.allCases.filter { !savedCLISet.contains($0) }
        let cliRank = Dictionary(uniqueKeysWithValues: orderedCLIs.enumerated().map { ($1, $0) })
        var ordered = merged.enumerated()
            .sorted { lhs, rhs in
                let lhsRank = SetupTool.detect(in: lhs.element.command).flatMap { cliRank[$0] } ?? Int.max
                let rhsRank = SetupTool.detect(in: rhs.element.command).flatMap { cliRank[$0] } ?? Int.max
                if lhsRank != rhsRank { return lhsRank < rhsRank }
                return lhs.offset < rhs.offset
            }
            .map(\.element)

        // Hoist each explicitly-chosen default above its CLI siblings.
        for (cliRaw, presetID) in legacyDefaults {
            guard let cli = SetupTool(rawValue: cliRaw),
                  let from = ordered.firstIndex(where: { $0.id == presetID }),
                  let to = ordered.firstIndex(where: { SetupTool.detect(in: $0.command) == cli }),
                  to < from
            else { continue }
            let preset = ordered.remove(at: from)
            ordered.insert(preset, at: to)
        }

        presetOrder = ordered.map(\.id)
        savePresetOrder()
        // The legacy keys are deliberately NOT deleted: UserDefaults is
        // shared by bundle id, so an older build running side by side (the
        // installed release app next to a dev build) would lose its CLI
        // order/defaults/hidden state mid-flight. The presetOrder-key guard
        // above already makes this migration one-shot.
    }

    // MARK: - Per-CLI default preset (order-derived)

    /// The preset the Unpeel Sessions MCP launches for a new session of `cli`:
    /// the topmost preset of that CLI in the flat list order (reordering the
    /// list is how the default is chosen).
    func defaultPreset(for cli: SetupTool) -> Preset? {
        mergedPresets.first { SetupTool.detect(in: $0.command) == cli }
    }

    func isDefaultPreset(_ preset: Preset, for cli: SetupTool) -> Bool {
        defaultPreset(for: cli)?.id == preset.id
    }

    // MARK: - Preset editing (shared app-state.json; PresetsPanel.svelte
    // semantics — legacy overlay paths only until the one-shot migration)

    /// add_preset (preset.rs:170-197) via PresetsPanel handleAdd: label =
    /// command, enabled, not quick-launch. `label` overrides the mirrored
    /// command — used when adding an installed App, whose display name reads
    /// better than its bare launch command.
    @discardableResult
    func addPreset(command: String, label: String? = nil) -> Preset? {
        let cmd = command.trimmingCharacters(in: .whitespaces)
        guard !cmd.isEmpty else { return nil }
        let trimmedLabel = label?.trimmingCharacters(in: .whitespaces)
        let displayLabel = (trimmedLabel?.isEmpty == false) ? trimmedLabel! : cmd
        let preset = Preset(
            id: "native-\(UUID().uuidString.lowercased())",
            label: displayLabel,
            command: cmd,
            enabled: true,
            quickLaunch: false
        )
        if presetsInSharedFile {
            let wrote = editPresetStateAnnouncing { object in
                var list = (object["presets"] as? [Any]) ?? []
                list.append(PresetStateFile.apply(preset, to: ["project_id": NSNull()]))
                object["presets"] = list
            }
            guard wrote else { return nil }
        } else {
            var overlay = loadPresetOverlay()
            overlay.added.append(preset)
            savePresetOverlay(overlay)
        }
        rescan()
        return preset
    }

    // MARK: - Installed Apps (launch-list "Apps you can add")

    /// Installed Unpeel Apps read from `unpeel-host __apps__ list`. Rust owns
    /// the central catalog + PATH resolution; native does not duplicate it.
    /// Refreshed on a throttle from `rescan()`.
    @Published private(set) var installedApps: [InstalledAppInfo] = []
    private var installedAppsRefreshInFlight = false
    private var installedAppsRefreshedAt: Date?
    /// Apps are installed rarely, so a background probe every few seconds is
    /// plenty and keeps `rescan()` cheap.
    private static let installedAppsRefreshInterval: TimeInterval = 5

    /// Installed apps not already present in the launch list (matched on the
    /// exact launch command), so the menu only offers apps you haven't added.
    /// Local scope only — remote presets flow through the Host protocol.
    var addableApps: [InstalledAppInfo] {
        guard selectedHostScope == .local else { return [] }
        let existing = Set(
            availablePresets.map { $0.command.trimmingCharacters(in: .whitespaces) }
        )
        return installedApps.filter {
            !existing.contains($0.command.trimmingCharacters(in: .whitespaces))
        }
    }

    /// Add an installed App to the launch list as a preset (label = app name),
    /// then refresh so it drops out of "Apps you can add".
    func addAppPreset(_ app: InstalledAppInfo) {
        addPreset(command: app.command, label: app.name)
        installedAppsRefreshedAt = nil
        refreshInstalledApps()
    }

    /// Background-probe the installed-app registry, throttled. Safe to call
    /// often (from `rescan()`); at most one probe runs at a time.
    func refreshInstalledApps(force: Bool = false) {
        if !force,
           let at = installedAppsRefreshedAt,
           Date().timeIntervalSince(at) < Self.installedAppsRefreshInterval {
            return
        }
        guard !installedAppsRefreshInFlight else { return }
        installedAppsRefreshInFlight = true
        installedAppsRefreshedAt = Date()
        DispatchQueue.global(qos: .utility).async { [weak self] in
            let apps = Self.readInstalledApps()
            DispatchQueue.main.async {
                guard let self else { return }
                self.installedAppsRefreshInFlight = false
                if self.installedApps != apps { self.installedApps = apps }
            }
        }
    }

    private static func readInstalledApps() -> [InstalledAppInfo] {
        let process = Process()
        process.executableURL = URL(fileURLWithPath: LaunchConfig.hostBinary)
        process.arguments = ["__apps__", "list"]
        let pipe = Pipe()
        process.standardOutput = pipe
        process.standardError = FileHandle.nullDevice
        guard (try? process.run()) != nil else { return [] }
        let data = pipe.fileHandleForReading.readDataToEndOfFile()
        process.waitUntilExit()
        guard process.terminationStatus == 0,
              let apps = try? JSONDecoder().decode([InstalledAppInfo].self, from: data)
        else { return [] }
        return apps
    }

    // MARK: - Agents (add removed / install missing)

    /// Installed agent CLIs (on PATH) that aren't already in the launch list —
    /// one click re-adds a removed agent. Local scope only; remote scope adds
    /// agents through the Host protocol (Phase 3).
    var addableAgents: [SetupTool] {
        guard selectedHostScope == .local, let report = setupToolReport else { return [] }
        let existing = Set(mergedPresets.compactMap { SetupTool.detect(in: $0.command) })
        return report.installedStatuses.map(\.tool).filter { !existing.contains($0) }
    }

    /// Agent CLIs not on PATH that we know how to install (have a trusted
    /// install one-liner in the runtime catalog). Local scope only.
    var installableAgents: [SetupTool] {
        guard selectedHostScope == .local, let report = setupToolReport else { return [] }
        return report.missingStatuses.map(\.tool).filter { $0.installCommand != nil }
    }

    /// Missing agent CLIs with no trusted install command — offer their vendor
    /// page (a guessed package name could install a squatted lookalike).
    var gettableAgents: [SetupTool] {
        guard selectedHostScope == .local, let report = setupToolReport else { return [] }
        return report.missingStatuses
            .map(\.tool)
            .filter { $0.installCommand == nil && $0.websiteURL != nil }
    }

    /// Add an installed agent back to the launch list as its default preset.
    func addAgentPreset(_ tool: SetupTool) {
        addPreset(command: tool.defaultPresetCommand, label: tool.displayName)
    }

    /// Install a missing agent by running its install one-liner as a visible
    /// terminal session on the currently-scoped host. Locally that opens a
    /// terminal on this Mac; the same `launchSession` path carries the command
    /// to a remote Host too. No trusted command → open the vendor page.
    func installAgentSession(_ tool: SetupTool) {
        guard let command = tool.installCommand else {
            if let url = tool.websiteURL { NSWorkspace.shared.open(url) }
            return
        }
        guard let projectID = defaultLaunchProjectID else { return }
        launchSession(projectID: projectID, command: command)
    }

    /// Install a missing agent on a REMOTE Host, as seamlessly as possible: the
    /// catalog one-liners assume a user-writable environment that a fresh Linux
    /// Host lacks, so the command is wrapped to (1) use a user-owned npm prefix
    /// — `npm install -g` needs no sudo — plus `~/.local/bin`, persisted to
    /// `~/.profile` so later sessions find the binary; (2) auto-install Node via
    /// NodeSource when an npm command runs and npm is missing (sudo when
    /// available). Runs as a visible session ON the Host.
    func installAgentOnRemote(_ tool: SetupTool, projectID: String) {
        guard let install = tool.installCommand else {
            if let url = tool.websiteURL { NSWorkspace.shared.open(url) }
            return
        }
        launchSession(projectID: projectID, command: Self.seamlessRemoteInstallCommand(install))
    }

    /// Base64-wrap the env-prepped install script so no quoting can break it
    /// over the wire; the Host decodes and runs it in a login shell.
    static func seamlessRemoteInstallCommand(_ install: String) -> String {
        var lines = [
            "set -e",
            "export NPM_CONFIG_PREFIX=\"$HOME/.npm-global\"",
            "mkdir -p \"$NPM_CONFIG_PREFIX/bin\"",
            "export PATH=\"$NPM_CONFIG_PREFIX/bin:$HOME/.local/bin:$PATH\"",
            "if ! grep -q 'unpeel agent PATH' \"$HOME/.profile\" 2>/dev/null; then "
                + "{ echo '# unpeel agent PATH'; "
                + "echo 'export NPM_CONFIG_PREFIX=\"$HOME/.npm-global\"'; "
                + "echo 'export PATH=\"$NPM_CONFIG_PREFIX/bin:$HOME/.local/bin:$PATH\"'; "
                + "} >> \"$HOME/.profile\"; fi",
        ]
        if install.hasPrefix("npm") {
            lines.append(
                "if ! command -v npm >/dev/null 2>&1; then echo '== installing Node.js =='; "
                    + "if command -v sudo >/dev/null 2>&1; then "
                    + "curl -fsSL https://deb.nodesource.com/setup_22.x | sudo -E bash - "
                    + "&& sudo apt-get install -y nodejs; "
                    + "else echo 'npm is missing and sudo is unavailable — install Node first'; fi; fi"
            )
        }
        lines.append(install)
        let b64 = Data(lines.joined(separator: "\n").utf8).base64EncodedString()
        return "echo \(b64) | base64 -d | bash -l"
    }

    /// update_preset (preset.rs:222-285), evaluated on the MERGED view and
    /// recorded into the overlay. Any number of presets can be starred —
    /// same-CLI stars collapse into one quick-strip dropdown chip, so there
    /// is no sibling-disable rule anymore.
    func updatePreset(
        id: String,
        command: String? = nil,
        quickLaunch: Bool? = nil
    ) {
        guard var preset = mergedPresets.first(where: { $0.id == id }) else { return }

        if let command {
            let cmd = command.trimmingCharacters(in: .whitespaces)
            guard !cmd.isEmpty else { return }
            preset.command = cmd
            // PresetsPanel keeps the label mirrored to the command.
            preset.label = cmd
        }
        // Keep the compatibility field true whenever this client touches a
        // preset. The product state is present-or-deleted.
        preset.enabled = true
        if let quickLaunch { preset.quickLaunch = quickLaunch }
        preset = preset.sanitized()

        if presetsInSharedFile {
            let updated = preset
            editPresetStateAnnouncing { object in
                object["presets"] = PresetStateFile.rawPresets(of: object).map { dict in
                    (dict["id"] as? String) == id
                        ? PresetStateFile.apply(updated, to: dict)
                        : dict
                }
            }
        } else {
            var overlay = loadPresetOverlay()
            record(preset, into: &overlay)
            savePresetOverlay(overlay)
        }
        rescan()
    }

    /// remove_preset (preset.rs:200-220). Migrated installs delete the row
    /// from the shared file; legacy installs drop natively-added presets from
    /// the overlay and tombstone file-based ones.
    func removePreset(id: String) {
        if presetsInSharedFile {
            editPresetStateAnnouncing { object in
                object["presets"] = PresetStateFile.rawPresets(of: object)
                    .filter { ($0["id"] as? String) != id }
            }
            rescan()
            return
        }
        var overlay = loadPresetOverlay()
        if let index = overlay.added.firstIndex(where: { $0.id == id }) {
            overlay.added.remove(at: index)
        } else {
            overlay.edited.removeAll { $0.id == id }
            if !overlay.removedIDs.contains(id) {
                overlay.removedIDs.append(id)
            }
        }
        savePresetOverlay(overlay)
        if presetOrder.contains(id) {
            presetOrder.removeAll { $0 == id }
            savePresetOrder()
        }
        rescan()
    }

    private func record(_ preset: Preset, into overlay: inout PresetOverlay) {
        if let index = overlay.added.firstIndex(where: { $0.id == preset.id }) {
            overlay.added[index] = preset
        } else {
            overlay.edited.removeAll { $0.id == preset.id }
            overlay.edited.append(preset)
        }
    }

    // MARK: - Native-added projects

    /// Projects added from the native footer "+" (Sidebar.svelte footer →
    /// App.svelte handleAddProject) plus natively-created worktree child
    /// projects (ensure_worktree_project parity). The Tauri app owns
    /// app-state.json, so native additions are persisted in UserDefaults
    /// and merged at read time, mirroring the pin-overrides approach.
    /// The worktree fields are optional so records written by older builds
    /// keep decoding.
    /// Internal (not private) so the pure adoption reconciliation above can
    /// be unit-tested; nothing outside the store constructs these otherwise.
    struct NativeProjectRecord: Codable, Equatable {
        let id: String
        var name: String
        let path: String
        var parentProjectID: String?
        var worktreeBranch: String?
        /// True only for linked worktrees adopted automatically from Git.
        /// This provenance lets discovery forget a child after another tool
        /// removes it without touching explicitly created/adopted projects.
        var autoDiscoveredWorktree: Bool?
        /// Plain group child folders (parent set, no branch); optional so
        /// records written by older builds keep decoding.
        var isFolder: Bool?
    }

    private static let nativeProjectsKey = "unpeel.native.projects"

    private func loadNativeProjects() -> [NativeProjectRecord] {
        Self.loadNativeProjects(from: AppDefaults.shared)
    }

    /// Native project records from an explicit defaults suite — the scoped
    /// workspace's own suite for a `.localWorkspace` Add Project/Remove.
    private static func loadNativeProjects(
        from defaults: UserDefaults
    ) -> [NativeProjectRecord] {
        guard let data = defaults.data(forKey: nativeProjectsKey),
              let records = try? JSONDecoder().decode([NativeProjectRecord].self, from: data)
        else { return [] }
        return records
    }

    private func nativeProjects(
        excludingPaths existing: Set<String>, excludingIDs existingIDs: Set<String> = []
    ) -> [Project] {
        let normalizedExisting = Set(existing.map(Self.normalizedProjectPath))
        return loadNativeProjects()
            .filter { record in
                // The file's copy of a mirrored record wins (same id).
                guard !existingIDs.contains(record.id) else { return false }
                // Child records (groups share the parent's path by design)
                // skip the path dedup; top-level records dedupe by path.
                return record.parentProjectID != nil
                    || !normalizedExisting.contains(Self.normalizedProjectPath(record.path))
            }
            .map { record in
                Project(
                    id: record.id,
                    name: record.name,
                    path: record.path,
                    parentProjectID: record.parentProjectID,
                    sortOrder: Int.max, // append after Tauri-ordered projects
                    isFolder: record.isFolder,
                    worktreeBranch: record.worktreeBranch,
                    workspacesEnabled: nil,
                    mcpBlocked: nil
                )
            }
    }

    /// Reconcile provider-created linked worktrees into the shared Project
    /// tree. Git itself is the relationship proof: no `.claude`, `.codex`, or
    /// other provider path is trusted or required.
    ///
    /// Returns true only when native records changed. A failed/non-main Git
    /// listing is deliberately absent from `listedPathsByParent`, so a
    /// transient error can never erase a previously discovered folder.
    private func syncLinkedWorktreeProjects(from projects: [Project]) -> Bool {
        guard selectedHostScope == .local, isExperimentalEnabled(.worktrees)
        else { return false }
        guard showAgentWorktrees else {
            // Setting off (the default): previously adopted provider
            // worktrees leave the sidebar. Cheap when there is nothing to
            // purge, and the one purge that changes records schedules its
            // own follow-up rescan.
            if purgeAutoDiscoveredWorktreeRecords() { scheduleRescan(after: 0) }
            return false
        }
        guard !linkedWorktreeDiscoveryInFlight,
              Date().timeIntervalSince(lastLinkedWorktreeDiscoveryAt) >= 5
        else { return false }
        lastLinkedWorktreeDiscoveryAt = Date()

        let topLevel = projects.filter {
            $0.parentProjectID == nil && $0.isFolder != true
        }
        guard !topLevel.isEmpty else { return false }
        // Each listing spawns a `git worktree list` subprocess. Doing that
        // inline blocked the main queue for hundreds of milliseconds per
        // rescan — and ghostty presents every terminal frame via a
        // main-queue hop, so those bursts read as terminal FPS drops. List
        // on a background queue and fold results in on main; the sync
        // return value is always false now, changes arrive via the
        // follow-up rescan the apply step schedules.
        linkedWorktreeDiscoveryInFlight = true
        let repos = topLevel.map { (id: $0.id, path: $0.path) }
        DispatchQueue.global(qos: .utility).async { [weak self] in
            var listed: [String: [WorktreeGit.LinkedWorktree]] = [:]
            for repo in repos {
                if let linked = WorktreeGit.linkedWorktrees(repoPath: repo.path) {
                    listed[repo.id] = linked
                }
            }
            Task { @MainActor [weak self] in
                guard let self else { return }
                self.linkedWorktreeDiscoveryInFlight = false
                // Reconcile against the tree the pass was started from (the
                // latest projection), not the launch-frozen scan: as a Host
                // client that scan never updates.
                if self.applyLinkedWorktreeDiscovery(listed: listed, projects: projects) {
                    self.scheduleRescan(after: 0)
                }
            }
        }
        return false
    }

    /// Fold a background discovery pass into native records. `listed` holds
    /// only repos whose Git listing succeeded, so a transient failure can
    /// never erase a previously discovered folder. The project tree is
    /// re-derived from the latest scan at apply time — records may have
    /// moved while git ran.
    private func applyLinkedWorktreeDiscovery(
        listed: [String: [WorktreeGit.LinkedWorktree]],
        projects: [Project]
    ) -> Bool {
        // The toggle may have flipped off while git ran; adopting now would
        // undo the purge the flip performed.
        guard showAgentWorktrees,
              let records = Self.reconcilingAutoDiscoveredWorktrees(
                  listed: listed,
                  projects: projects,
                  records: loadNativeProjects()
              ),
              let data = try? JSONEncoder().encode(records)
        else { return false }
        AppDefaults.shared.set(data, forKey: Self.nativeProjectsKey)
        mirrorProjectsToSharedState()
        return true
    }

    /// Pure reconciliation of one discovery pass: adopt each linked worktree
    /// a top-level project's Git registry reports, maintain the fields
    /// automatic discovery owns on already-adopted rows, and forget adopted
    /// rows Git no longer lists. Returns the new record set, or nil when
    /// nothing changed. Explicitly registered worktrees are never touched,
    /// and a parent missing from `listed` (its listing failed) keeps every
    /// child it already had.
    nonisolated static func reconcilingAutoDiscoveredWorktrees(
        listed: [String: [WorktreeGit.LinkedWorktree]],
        projects: [Project],
        records: [NativeProjectRecord]
    ) -> [NativeProjectRecord]? {
        let topLevel = projects.filter {
            $0.parentProjectID == nil && $0.isFolder != true
        }
        let parentIDs = Set(topLevel.map(\.id))
        var records = records
        var changed = false
        var listedPathsByParent: [String: Set<String>] = [:]
        var knownPaths = Set(projects.map { Self.normalizedProjectPath($0.path) })

        for parent in topLevel {
            guard let linked = listed[parent.id] else {
                continue
            }
            let linkedPaths = Set(linked.map { Self.normalizedProjectPath($0.path) })
            listedPathsByParent[parent.id] = linkedPaths

            for worktree in linked {
                let path = Self.normalizedProjectPath(worktree.path)
                if let index = records.firstIndex(where: {
                    Self.normalizedProjectPath($0.path) == path
                }) {
                    // Only maintain fields owned by automatic discovery.
                    // Explicitly registered worktrees retain their chosen
                    // parent, branch metadata, and user-facing name.
                    if records[index].autoDiscoveredWorktree == true {
                        if records[index].parentProjectID != parent.id {
                            records[index].parentProjectID = parent.id
                            changed = true
                        }
                        if records[index].worktreeBranch != worktree.branch {
                            records[index].worktreeBranch = worktree.branch
                            changed = true
                        }
                    }
                    continue
                }
                // The path may already be owned by app-state.json or an
                // ephemeral verification project instead of native records.
                guard !knownPaths.contains(path) else { continue }

                let identity = "\(parent.id)\u{1f}\(path)"
                let hash = String(WorktreeGit.fnv1aHash(identity), radix: 16)
                let id = "native-auto-worktree-\(hash)"
                let folder = URL(fileURLWithPath: path).lastPathComponent
                let parentFolder = URL(fileURLWithPath: parent.path).lastPathComponent
                let name = folder.isEmpty
                    || folder.caseInsensitiveCompare(parentFolder) == .orderedSame
                    ? worktree.branch : folder
                records.append(NativeProjectRecord(
                    id: id,
                    name: name,
                    path: path,
                    parentProjectID: parent.id,
                    worktreeBranch: worktree.branch,
                    autoDiscoveredWorktree: true
                ))
                knownPaths.insert(path)
                changed = true
            }
        }

        // Only successful Git listings authorize stale-child cleanup. If a
        // parent itself disappeared, its automatically owned children are
        // stale too; explicit worktree records remain untouched.
        records.removeAll { record in
            guard record.autoDiscoveredWorktree == true,
                  let parentID = record.parentProjectID
            else { return false }
            let path = Self.normalizedProjectPath(record.path)
            let stale = !parentIDs.contains(parentID)
                || listedPathsByParent[parentID].map { !$0.contains(path) } == true
            if stale { changed = true }
            return stale
        }

        return changed ? records : nil
    }

    /// Records without every automatically adopted worktree, or nil when
    /// there was none to drop. Explicit worktrees and groups stay.
    nonisolated static func purgingAutoDiscoveredWorktrees(
        _ records: [NativeProjectRecord]
    ) -> [NativeProjectRecord]? {
        let kept = records.filter { $0.autoDiscoveredWorktree != true }
        return kept.count == records.count ? nil : kept
    }

    /// Drop every automatically adopted provider worktree record (Settings ▸
    /// Worktrees "Show agent worktrees" off). Explicitly registered
    /// worktrees keep their records; the mirror write removes the purged
    /// rows from the shared `app-state.json`, so the TUI matches.
    private func purgeAutoDiscoveredWorktreeRecords() -> Bool {
        guard let records = Self.purgingAutoDiscoveredWorktrees(loadNativeProjects()),
              let data = try? JSONEncoder().encode(records)
        else { return false }
        AppDefaults.shared.set(data, forKey: Self.nativeProjectsKey)
        mirrorProjectsToSharedState()
        return true
    }

    /// Footer "+" — Add Project (Sidebar.svelte:568-581 → App.svelte
    /// handleAddProject:1099-1120): folder picker, then the project appears
    /// in the tree. Stored natively; never written to app-state.json.
    func addProjectFolder() {
        guard selectedHostScope.isLocalMachine else { return }
        let panel = NSOpenPanel()
        panel.canChooseDirectories = true
        panel.canChooseFiles = false
        panel.allowsMultipleSelection = false
        panel.title = "Select project folder"
        panel.prompt = "Add Project"
        panel.begin { [weak self] response in
            guard response == .OK, let url = panel.url else { return }
            Task { @MainActor in
                // Open the newly added project's launcher by default.
                self?.openLauncher(forFolder: url.path)
            }
        }
    }

    @discardableResult
    func addProjectFolders(from providers: [NSItemProvider]) -> Bool {
        guard selectedHostScope.isLocalMachine else { return false }
        let fileURLProviders = providers.filter {
            $0.hasItemConformingToTypeIdentifier(UTType.fileURL.identifier)
        }
        guard !fileURLProviders.isEmpty else { return false }

        for provider in fileURLProviders {
            provider.loadItem(
                forTypeIdentifier: UTType.fileURL.identifier,
                options: nil
            ) { [weak self] item, _ in
                guard let url = Self.fileURL(fromDropItem: item) else { return }
                Task { @MainActor [weak self] in
                    self?.addDroppedProjectFolder(url)
                }
            }
        }

        return true
    }

    /// Reuse-or-add a permanent native project for `path`, returning its id.
    /// Reuses any existing project (app-state, native, or worktree) whose
    /// path matches; otherwise adds a permanent native project like the
    /// "+ Add Project" button (App.svelte:1113-1118 "already added" guard).
    @discardableResult
    func ensureProject(path: String) -> String {
        if selectedHostScope.scopedLocalHome != nil {
            return ensureScopedWorkspaceProject(path: path)
        }
        let normalizedPath = Self.normalizedProjectPath(path)
        if let existing = projectsByID.values.first(where: {
            Self.normalizedProjectPath($0.path) == normalizedPath
        }) {
            return existing.id
        }
        var records = loadNativeProjects()
        if let existing = records.first(where: {
            Self.normalizedProjectPath($0.path) == normalizedPath
        }) {
            return existing.id
        }
        let id = "native-\(UUID().uuidString.lowercased())"
        records.append(NativeProjectRecord(
            id: id,
            name: URL(fileURLWithPath: normalizedPath).lastPathComponent,
            path: normalizedPath
        ))
        if let data = try? JSONEncoder().encode(records) {
            AppDefaults.shared.set(data, forKey: Self.nativeProjectsKey)
            mirrorProjectsToSharedState()
        }
        rescan()
        return id
    }

    /// `.localWorkspace` Add Project: reuse-or-add against the SCOPED
    /// workspace's own home (its UserDefaults suite + its `app-state.json`),
    /// never this instance's. The gateway reads that home's `app-state.json`,
    /// so the added project surfaces in the scoped sidebar on the nudged
    /// re-bootstrap. Session hosting still rides the gateway — this only files
    /// the project, it never spawns anything locally.
    @discardableResult
    private func ensureScopedWorkspaceProject(path: String) -> String {
        let normalizedPath = Self.normalizedProjectPath(path)
        // Already in the scoped projection (any frontend owns it): reuse it.
        if let existing = displayProjectsByID.values.first(where: {
            Self.normalizedProjectPath($0.path) == normalizedPath
        }) {
            return existing.id
        }
        let defaults = scopedAppDefaults
        var records = Self.loadNativeProjects(from: defaults)
        if let existing = records.first(where: {
            Self.normalizedProjectPath($0.path) == normalizedPath
        }) {
            return existing.id
        }
        let id = "native-\(UUID().uuidString.lowercased())"
        let record = NativeProjectRecord(
            id: id,
            name: URL(fileURLWithPath: normalizedPath).lastPathComponent,
            path: normalizedPath
        )
        records.append(record)
        if let data = try? JSONEncoder().encode(records) {
            defaults.set(data, forKey: Self.nativeProjectsKey)
        }
        // The gateway only reads app-state.json — mirror the record there
        // under the shared lock, then announce + nudge the re-bootstrap.
        editScopedAppStateAnnouncing { object in
            var projects = (object["projects"] as? [[String: Any]]) ?? []
            let dead = projects.contains { entry in
                (entry["id"] as? String) == id
                    || (entry["path"] as? String).map {
                        Self.normalizedProjectPath($0) == normalizedPath
                    } == true
            }
            guard !dead else { return }
            projects.append([
                "id": record.id, "name": record.name, "path": record.path,
            ])
            object["projects"] = projects
        }
        return id
    }

    private func addDroppedProjectFolder(_ url: URL) {
        // The asynchronous NSItemProvider load may complete after the user
        // switches Hosts. Recheck at the mutation boundary, not only when
        // the drop was accepted.
        guard selectedHostScope.isLocalMachine else { return }
        guard url.isFileURL else { return }
        let path = Self.normalizedProjectPath(url.path)
        var isDirectory = ObjCBool(false)
        guard FileManager.default.fileExists(atPath: path, isDirectory: &isDirectory),
              isDirectory.boolValue
        else { return }

        let projectID = ensureProject(path: path)
        settingsVisible = false
        archivedProjectID = nil
        expandedProjectIDs.insert(projectID)
    }

    nonisolated private static func normalizedProjectPath(_ path: String) -> String {
        URL(fileURLWithPath: path)
            .standardizedFileURL
            .resolvingSymlinksInPath()
            .path
    }

    nonisolated private static func fileURL(fromDropItem item: NSSecureCoding?) -> URL? {
        if let url = item as? URL {
            return url
        }
        if let data = item as? Data,
           let string = String(data: data, encoding: .utf8) {
            return URL(string: string)
        }
        if let string = item as? String {
            if let url = URL(string: string) {
                return url
            }
            return URL(fileURLWithPath: string)
        }
        return nil
    }

    /// Finder "New Unpeel Session Here" service entry point: reuse-or-add the
    /// project for `path`, then show the main-screen session launcher for it
    /// (the user picks a tool there). The launcher gives way to the terminal
    /// as soon as a tile is launched.
    func openLauncher(forFolder path: String) {
        // Covers Finder Services, the folder picker, and delayed callbacks.
        // A true remote scope must never inspect/mirror a Controller-local
        // path; a `.localWorkspace` files the project against its scoped home
        // (same machine) and the gateway surfaces it.
        guard selectedHostScope.isLocalMachine else { return }
        let projectID = ensureProject(path: path)
        settingsVisible = false
        archivedProjectID = nil
        selectedSessionID = nil
        expandedProjectIDs.insert(projectID)
        launcherProjectID = projectID
    }

    /// Project context-menu destination for archived sessions. This is a
    /// main-pane library, not another sidebar accordion.
    func openArchivedSessions(projectID: String) {
        guard displayProjectsByID[projectID] != nil else { return }
        settingsVisible = false
        launcherProjectID = nil
        recentActivityVisible = false
        archivedProjectID = projectID
        // Remote projects fetch their archive library from the Host
        // (`session.archive.list`); the page renders from the cached fetch.
        if remoteProjectSummariesByID[projectID] != nil {
            refreshRemoteArchivedSessions(projectID: projectID)
        }
    }

    func closeArchivedSessions() {
        archivedProjectID = nil
    }

    /// "All recent" destination from the titlebar/menu-bar activity
    /// dropdowns: the app-wide history page in the main pane, same shell as
    /// the archived-sessions library.
    func openRecentActivity() {
        guard selectedHostScope == .local else { return }
        settingsVisible = false
        launcherProjectID = nil
        archivedProjectID = nil
        recentActivityVisible = true
    }

    func closeRecentActivity() {
        recentActivityVisible = false
    }

    func toggleRecentActivity() {
        if recentActivityVisible {
            closeRecentActivity()
        } else {
            openRecentActivity()
        }
    }

    // MARK: - Ephemeral projects (verification only)

    /// In-memory projects for hook verification (UNPEEL_TEST_LAUNCH with a
    /// `path:` spec). Never persisted anywhere, so other Unpeel instances
    /// and future launches see no residue.
    private var ephemeralProjects: [Project] = []

    @discardableResult
    func addEphemeralProject(path: String) -> String {
        if let existing = ephemeralProjects.first(where: { $0.path == path }) {
            return existing.id
        }
        // Deterministic id so sessions launched into this project in an
        // earlier run still resolve to it after an app restart.
        let slug = path.lowercased().map { $0.isLetter || $0.isNumber ? $0 : "-" }
        let id = "ephemeral-\(String(slug))"
        ephemeralProjects.append(Project(
            id: id,
            name: URL(fileURLWithPath: path).lastPathComponent,
            path: path,
            parentProjectID: nil,
            sortOrder: Int.max,
            isFolder: nil,
            worktreeBranch: nil,
            workspacesEnabled: nil,
            mcpBlocked: nil
        ))
        rescan()
        return id
    }

    /// Ephemeral worktree CHILD project (verification of the worktrees link
    /// row / spinner without touching any real project). Adding the child is
    /// enough for the parent's link row to render — worktrees show purely by
    /// existence. In-memory only, like addEphemeralProject.
    @discardableResult
    func addEphemeralWorktreeProject(
        parentPath: String, path: String, branch: String
    ) -> String {
        let parentID = addEphemeralProject(path: parentPath)
        if let existing = ephemeralProjects.first(where: { $0.path == path }) {
            return existing.id
        }
        let slug = path.lowercased().map { $0.isLetter || $0.isNumber ? $0 : "-" }
        let id = "ephemeral-\(String(slug))"
        ephemeralProjects.append(Project(
            id: id,
            name: URL(fileURLWithPath: path).lastPathComponent,
            path: path,
            parentProjectID: parentID,
            sortOrder: Int.max,
            isFolder: nil,
            worktreeBranch: branch,
            workspacesEnabled: nil,
            mcpBlocked: nil
        ))
        rescan()
        return id
    }

    // MARK: - Derived

    var selectedSession: SessionEntry? {
        selectedSessionID.flatMap { displaySessionsByID[$0] }
    }

    /// Selection painted into the current instance's read-only sidebar page
    /// while another workspace is foregrounded. The live Local tree restores
    /// this exact id on return, so the carousel must show its active chrome
    /// before the commit rather than adding it one frame afterward.
    var sidebarCarouselLocalSelectedSessionID: String? {
        selectedHostScope == .local
            ? selectedSessionID
            : localSelectedSessionIDBeforeRemote
    }

    /// Sessions that currently own visible work: row spinners show for
    /// starting/busy sessions, plus restart placeholders while a relaunch is
    /// in flight.
    var activeJobSessions: [SessionEntry] {
        var result: [SessionEntry] = []
        var seen = Set<String>()
        func append(_ session: SessionEntry) {
            guard !seen.contains(session.id) else { return }
            seen.insert(session.id)
            result.append(session)
        }
        func collect(_ nodes: [ProjectNode]) {
            for node in nodes {
                for session in node.sessions
                where session.status == .starting || session.status == .busy {
                    append(session)
                }
                collect(node.worktrees)
            }
        }
        collect(nodes)
        for id in restartingSessionIDs.sorted() {
            if let session = sessionsByID[id] {
                append(session)
            }
        }
        return Self.sessionsSortedByRecentActivity(
            result,
            restartingSessionIDs: restartingSessionIDs
        )
    }

    var activeJobCount: Int {
        activeJobSessions.count
    }

    /// Product-facing status word for the activity dropdowns (titlebar + menu
    /// bar). Rows may still choose to render this as a spinner or unread dot.
    func activityStatusLabel(for session: SessionEntry) -> String {
        if restartingSessionIDs.contains(session.id) {
            return session.isLive ? "Restarting" : "Resuming"
        }
        switch session.activityStatus(unread: unreadSessionIDs.contains(session.id)) {
        case .starting: return "Starting"
        case .working: return "Working"
        case .blocked: return "Blocked"
        case .done: return "Done"
        case .idle: return "Idle"
        case .exited: return "Exited"
        }
    }

    /// Display name for a project id, used by the activity dropdowns.
    /// Plain group folders carry the full path — Project › Folder — matching
    /// the titlebar; worktrees keep their own name (the branch identifies
    /// them elsewhere, and the parent prefix would just be noise here).
    func activityProjectName(_ id: String) -> String {
        guard let project = projectsByID[id] else { return "Unknown project" }
        if project.worktreeBranch == nil,
           let parentID = project.parentProjectID,
           let parent = projectsByID[parentID] {
            return "\(parent.name) › \(project.name)"
        }
        return project.name
    }

    /// Sessions with an unread settle or App alert (the #60a5fa dot) that are
    /// no longer doing visible work. Surfaced in the titlebar activity
    /// popover beneath active jobs, newest activity first.
    var unreadJobSessions: [SessionEntry] {
        let active = Set(activeJobSessions.map(\.id))
        var result: [SessionEntry] = []
        var seen = Set<String>()
        func collect(_ nodes: [ProjectNode]) {
            for node in nodes {
                for session in node.sessions
                where unreadSessionIDs.contains(session.id)
                    && !active.contains(session.id)
                    && !seen.contains(session.id) {
                    seen.insert(session.id)
                    result.append(session)
                }
                collect(node.worktrees)
            }
        }
        collect(nodes)
        return result.sorted { lhs, rhs in
            let lhsStamp = sessionRecencyMs(lhs.id)
            let rhsStamp = sessionRecencyMs(rhs.id)
            if lhsStamp != rhsStamp { return lhsStamp > rhsStamp }
            return lhs.id < rhs.id
        }
    }

    /// Project path for the terminal-header breadcrumb. Worktrees keep their
    /// branch in the compact icon suffix rendered after the breadcrumb.
    /// The project whose name/branch the titlebar shows: the selected
    /// session's project. Nil when nothing is selected — the titlebar then
    /// shows the app name with no branch.
    private var titlebarProject: Project? {
        guard let s = selectedSession else { return nil }
        return displayProjectsByID[s.projectID]
    }

    var titlebarSegments: [String] {
        // The workspace switcher lives in the sidebar, so retain the previous
        // terminal-header behavior: local workspaces all use the same project
        // breadcrumb. Only a genuinely remote Host needs a scope prefix.
        let hostPrefix: [String]
        if case .remote = selectedHostScope, let remoteName = remoteScopeDisplayName {
            hostPrefix = [remoteName]
        } else {
            hostPrefix = []
        }
        guard let project = titlebarProject else {
            return hostPrefix.isEmpty ? ["Unpeel"] : hostPrefix
        }
        // Worktree projects show only the parent name; the worktree itself
        // is identified by the muted branch suffix instead of its name.
        if let parentID = project.parentProjectID,
           let parent = displayProjectsByID[parentID] {
            if project.worktreeBranch != nil {
                return hostPrefix + [parent.name]
            }
            // A plain group folder has no branch suffix to identify it, so
            // the title carries the full path: Project › Group.
            return hostPrefix + [parent.name, project.name]
        }
        return hostPrefix + [project.name]
    }

    /// Guards async branch resolution against stale results after a switch.
    private var titlebarBranchPath: String?
    private var titlebarBranchRequestsInFlight: Set<String> = []
    private var titlebarBranchCache: [String: (branch: String?, resolvedAt: Date)] = [:]
    private static let titlebarBranchCacheTTL: TimeInterval = 5

    func refreshTitlebarBranch() {
        guard let project = titlebarProject else {
            titlebarBranchPath = nil
            titlebarBranchState.update(name: nil, isWorktree: false)
            return
        }
        // Worktree: branch is already in the model (local or projected),
        // no git call needed.
        if let branch = project.worktreeBranch {
            titlebarBranchPath = project.path
            titlebarBranchState.update(name: branch, isWorktree: true)
            return
        }
        // Local-machine scopes — this instance AND other Unpeel workspaces
        // on this Mac — have real checkout paths. Resolve HEAD locally.
        // The disk gateway's bootstrap historically omitted `gitBranch`,
        // so trusting a Host summary here left scoped workspaces blank.
        if selectedHostScope.isLocalMachine {
            resolveLocalTitlebarBranch(path: project.path)
            return
        }
        // True remote Host: the Host resolved the branch already; a
        // Controller must never run git against a Host-side path.
        if let summary = remoteProjectSummariesByID[project.id] {
            titlebarBranchPath = nil
            let isWorktree = summary.worktreeBranch != nil
            let branch = summary.worktreeBranch ?? summary.gitBranch
            titlebarBranchState.update(name: branch, isWorktree: isWorktree)
            return
        }
        titlebarBranchPath = nil
        titlebarBranchState.update(name: nil, isWorktree: false)
    }

    /// Plain git project on this Mac: immediately reuse a recently resolved
    /// result. Selection ping-pong previously launched one git process per
    /// click; a short cache plus one in-flight request per path bounds that
    /// work.
    private func resolveLocalTitlebarBranch(path: String) {
        guard UnpeelStore.isGitRepo(path: path) else {
            titlebarBranchPath = nil
            titlebarBranchState.update(name: nil, isWorktree: false)
            return
        }
        titlebarBranchPath = path
        if let cached = titlebarBranchCache[path] {
            titlebarBranchState.update(name: cached.branch, isWorktree: false)
            if Date().timeIntervalSince(cached.resolvedAt) < Self.titlebarBranchCacheTTL {
                return
            }
        } else {
            titlebarBranchState.update(name: nil, isWorktree: false)
        }
        guard titlebarBranchRequestsInFlight.insert(path).inserted else { return }
        DispatchQueue.global(qos: .userInitiated).async {
            let branch = WorktreeGit.currentBranch(repoPath: path)
            DispatchQueue.main.async { [weak self] in
                guard let self else { return }
                self.titlebarBranchRequestsInFlight.remove(path)
                self.titlebarBranchCache[path] = (branch, Date())
                guard self.titlebarBranchPath == path else { return }
                self.titlebarBranchState.update(name: branch, isWorktree: false)
            }
        }
    }

    // MARK: - Sidebar session lists (shared with ProjectNodeView)

    /// Whether this session is hidden from the regular sidebar lists.
    /// Archives whose host stop is still in flight stay visible (as a muted
    /// "archiving…" row) until the stop completes.
    private func isHiddenArchived(_ sessionID: String) -> Bool {
        // Remote rows: the Host already filtered/windowed the bootstrap, so
        // a Controller never applies its own local archive overlay to them.
        if remoteSummariesByID[sessionID] != nil { return false }
        return archivedSessionIDs.contains(sessionID)
            && !archivingSessionIDs.contains(sessionID)
    }

    /// Pins resolved against the node's sessions, mirroring
    /// resolvedPinnedItems in ProjectItem.svelte:405-423.
    func pinnedSessions(in node: ProjectNode) -> [SessionEntry] {
        // Remote nodes: pins are Host-resolved flags on the summaries, in
        // the Host's own row order; archive status overrides a retained pin.
        // An in-flight drag previews on top for the remaining pinned rows.
        if remoteProjectSummariesByID[node.id] != nil {
            let pinned = node.sessions.filter {
                remoteSummariesByID[$0.id]?.pinned == true
                    && !sessionIsRecentArchived($0.id)
            }
            guard let preview = sessionOrderPreviews[node.id] else { return pinned }
            var rank: [String: Int] = [:]
            for (index, id) in preview.enumerated() { rank[id] = index }
            return pinned.sorted {
                (rank[$0.id] ?? Int.max, $0.id) < (rank[$1.id] ?? Int.max, $1.id)
            }
        }
        return (pinnedByProject[node.id] ?? []).compactMap { pin in
            guard let id = pin.sessionID else { return nil }
            // Keep the durable pin so Restore can put the row back where the
            // user left it, but while archived it belongs to the fixed bottom
            // archive section rather than the pinned section.
            guard !sessionIsRecentArchived(id) else { return nil }
            return node.sessions.first { $0.id == id }
        }
    }

    /// The regular-only projection excludes pinned and archived rows.
    /// `sidebarLists` applies the shared inactive window after adding archived
    /// rows back; the project archive page exposes the complete filed set.
    func regularSessions(in node: ProjectNode) -> [SessionEntry] {
        let pinnedIDs = Set(pinnedSessions(in: node).map(\.id))
        return node.sessions.filter {
            !pinnedIDs.contains($0.id) && !isHiddenArchived($0.id)
        }
    }

    /// This project's archived sessions, newest-filed first. Manual order is
    /// deliberately ignored for Archive; in-flight archives are excluded
    /// until their host stop completes (they still render as a sidebar row).
    func archivedSessions(in node: ProjectNode) -> [SessionEntry] {
        node.sessions
            .filter { isHiddenArchived($0.id) }
            .sorted {
                let left = archiveSidebarRecency($0)
                let right = archiveSidebarRecency($1)
                return left == right ? $0.id < $1.id : left > right
            }
    }

    private func archiveSidebarRecency(_ session: SessionEntry) -> Int64 {
        max(
            max(session.createdAt, session.lifecycleAtMs ?? 0),
            max(
                archivedAtBySession[session.id] ?? 0,
                sidebarUnpinRecencyBump[session.id] ?? 0
            )
        )
    }

    func archivedSessions(projectID: String) -> [SessionEntry] {
        // Remote projects: the fetched Host archive library (empty until the
        // fetch lands or when the Host has none).
        if remoteProjectSummariesByID[projectID] != nil {
            return remoteArchivedByProject[projectID] ?? []
        }
        return localArchivedSessions(projectID: projectID)
    }

    /// Local-only archive lookup for the /mobile Host serving path, which
    /// must keep serving THIS Mac's archives even while a remote Host is
    /// selected in the picker.
    func localArchivedSessions(projectID: String) -> [SessionEntry] {
        guard let node = findNode(projectID) else { return [] }
        return archivedSessions(in: node)
    }

    /// The sidebar's row list for a group: pins first, then live sessions,
    /// then at most the configured number of naturally stopped and archived
    /// sessions combined (default five). Recently updated uses the shared
    /// lifecycle rank inside each section; Custom keeps manual order for live
    /// rows. Selected, unread, and in-flight inactive rows stay past the window.
    func displayedSessions(in node: ProjectNode) -> [SessionEntry] {
        sidebarLists(in: node).displayed
    }

    func renderedPinnedSessions(in node: ProjectNode) -> [SessionEntry] {
        sidebarLists(in: node).pinned
    }

    func renderedDisplayedSessions(in node: ProjectNode) -> [SessionEntry] {
        sidebarLists(in: node).displayed
    }

    /// The pinned partition as ONE mixed list: pinned sessions AND pinned
    /// child groups, ordered by the project's `pinned_sessions` records
    /// array (`rebuildPins` already applied the shared-order overlay to
    /// it). Records whose target is gone are skipped; a pinned group with
    /// no ordering record yet (pinned from the TUI, which writes only the
    /// `pinned_at` marker) ranks after every record-ranked row, keeping
    /// its current relative order. An in-flight drag preview re-ranks the
    /// assembled rows so recordless groups can ride the preview too.
    /// Remote nodes keep the shipped Host-resolved behavior: pinned
    /// sessions here, pinned groups first in the regular section.
    func renderedPinnedItems(in node: ProjectNode) -> [SidebarMixedItem] {
        let sessions = renderedPinnedSessions(in: node)
        if remoteProjectSummariesByID[node.id] != nil {
            return sessions.map { .session($0) }
        }
        let pinnedFolders = visibleChildFolders(in: node).filter {
            $0.project.acceptsSessionDrop && $0.project.pinnedAt != nil
        }
        guard !pinnedFolders.isEmpty else {
            return sessions.map { .session($0) }
        }
        var sessionsByRowID: [String: SessionEntry] = [:]
        for session in sessions { sessionsByRowID[session.id] = session }
        var foldersByID: [String: ProjectNode] = [:]
        for folder in pinnedFolders { foldersByID[folder.id] = folder }
        var items: [SidebarMixedItem] = []
        var seen = Set<String>()
        for pin in pinnedByProject[node.id] ?? [] {
            guard let targetID = pin.orderTargetID, !seen.contains(targetID)
            else { continue }
            if let folder = foldersByID[targetID] {
                seen.insert(targetID)
                items.append(.folder(folder))
            } else if let session = sessionsByRowID[targetID] {
                seen.insert(targetID)
                items.append(.session(session))
            }
        }
        // Defensive: a rendered pinned session must never drop out of the
        // partition even if its record is somehow missing.
        for session in sessions where seen.insert(session.id).inserted {
            items.append(.session(session))
        }
        for folder in pinnedFolders where seen.insert(folder.id).inserted {
            items.append(.folder(folder))
        }
        if let preview = sessionOrderPreviews[node.id] {
            var rank: [String: Int] = [:]
            for (index, id) in preview.enumerated() where rank[id] == nil {
                rank[id] = index
            }
            items = items.enumerated().sorted { lhs, rhs in
                let left = rank[lhs.element.id] ?? Int.max
                let right = rank[rhs.element.id] ?? Int.max
                return left == right ? lhs.offset < rhs.offset : left < right
            }.map(\.element)
        }
        return items
    }

    /// Shared session-order list to advertise on bootstrap when it actually
    /// interleaves a child group/worktree with sessions. Date-sorted
    /// projects keep the folders-first default unless a live drag preview
    /// is in flight.
    func advertisedSessionOrder(for projectID: String) -> [String]? {
        let folderIDs = Set(
            (findNode(projectID).map { visibleChildFolders(in: $0) } ?? []).map(\.id)
        )
        guard !folderIDs.isEmpty else { return nil }
        if isDateSorted(projectID: projectID), sessionOrderPreviews[projectID] == nil {
            return nil
        }
        let order = sessionOrderPreviews[projectID]
            ?? Self.sharedSessionOrder(projectID: projectID)
            ?? AppDefaults.shared.stringArray(forKey: Self.sessionOrderKey(projectID))
            ?? []
        guard order.contains(where: { folderIDs.contains($0) }) else { return nil }
        return order
    }

    /// Child folders shown in this node's regular section. Groups always
    /// appear; worktrees follow the experimental flag. The project "Sidebar"
    /// group never renders on the DESKTOP — its members live in the right
    /// panel here; the group row exists for the phone/TUI, which read the
    /// same shared state but have no right panel.
    func visibleChildFolders(in node: ProjectNode) -> [ProjectNode] {
        let worktreesOn = isExperimentalEnabled(.worktrees)
        return node.worktrees.filter {
            ((worktreesOn || $0.project.worktreeBranch == nil))
                && !isProjectSidebarGroup($0.id)
        }
    }

    /// Regular-section rows: sessions and child folders interleaved when
    /// the shared session-order list contains any folder id. Until a folder
    /// is dragged, folders stay above sessions (the previous layout).
    /// Local pinned child groups belong to the pinned partition
    /// (`renderedPinnedItems`) and never render here; remote nodes keep the
    /// shipped behavior of Host-flagged pinned groups sorting first.
    func renderedDisplayedItems(in node: ProjectNode) -> [SidebarMixedItem] {
        let sessions = renderedDisplayedSessions(in: node)
        let isRemoteNode = remoteProjectSummariesByID[node.id] != nil
        let folders = visibleChildFolders(in: node).filter { folder in
            isRemoteNode
                || !(folder.project.acceptsSessionDrop
                    && folder.project.pinnedAt != nil)
        }
        let pinnedGroupIDs: Set<String> = isRemoteNode
            ? Set(folders.compactMap { folder in
                remoteProjectSummariesByID[folder.id]?.pinned == true
                    ? folder.id
                    : nil
            })
            : []
        func pinnedGroupsFirst(_ items: [SidebarMixedItem]) -> [SidebarMixedItem] {
            guard !pinnedGroupIDs.isEmpty else { return items }
            let pinned = items.filter { item in
                if case .folder(let folder) = item {
                    return pinnedGroupIDs.contains(folder.id)
                }
                return false
            }
            return pinned + items.filter { item in
                if case .folder(let folder) = item {
                    return !pinnedGroupIDs.contains(folder.id)
                }
                return true
            }
        }
        // Folder/manual ordering applies only to the regular section.
        // Archived sessions have no drag slots and always form the final
        // newest-first section, after every folder and non-archived row.
        func archivedSessionsLast(_ items: [SidebarMixedItem]) -> [SidebarMixedItem] {
            let archived = items.filter { item in
                if case .session(let session) = item {
                    return sessionIsRecentArchived(session.id)
                }
                return false
            }
            return items.filter { item in
                if case .session(let session) = item {
                    return !sessionIsRecentArchived(session.id)
                }
                return true
            } + archived
        }
        guard !folders.isEmpty else {
            return sessions.map { .session($0) }
        }
        let dateSorted = isRemoteNode
            ? remoteProjectSummariesByID[node.id]?.dateSorted == true
            : dateSortedProjectIDs.contains(node.id)
        let order = dateSorted
            ? (sessionOrderPreviews[node.id] ?? [])
            : isRemoteNode
                ? sessionOrderPreviews[node.id]
                    ?? remoteSessionOrderByProject[node.id]
                    ?? []
                : sessionOrderPreviews[node.id]
                    ?? Self.sharedSessionOrder(projectID: node.id)
                    ?? AppDefaults.shared.stringArray(forKey: Self.sessionOrderKey(node.id))
                    ?? []
        let folderIDs = Set(folders.map(\.id))
        let mixed = order.contains { folderIDs.contains($0) }
        if (dateSorted && sessionOrderPreviews[node.id] == nil) || !mixed {
            return archivedSessionsLast(pinnedGroupsFirst(
                folders.map { .folder($0) } + sessions.map { .session($0) }
            ))
        }
        var byID: [String: SidebarMixedItem] = [:]
        for folder in folders { byID[folder.id] = .folder(folder) }
        for session in sessions { byID[session.id] = .session(session) }
        var seen = Set<String>()
        var ranked: [SidebarMixedItem] = []
        for id in order {
            if let item = byID[id], seen.insert(id).inserted {
                ranked.append(item)
            }
        }
        let unrankedSessions = sessions
            .filter { !seen.contains($0.id) }
            .sorted { $0.createdAt > $1.createdAt }
            .map { SidebarMixedItem.session($0) }
        let unrankedFolders = folders
            .filter { !seen.contains($0.id) }
            .map { SidebarMixedItem.folder($0) }
        return archivedSessionsLast(
            pinnedGroupsFirst(unrankedSessions + unrankedFolders + ranked)
        )
    }

    /// Flat session rows plus the inactive-only projection used for sidebar
    /// windowing. The array-of-arrays shape remains because this path
    /// historically operated on blocks; each block is one session now.
    private func sidebarSessionBlocks(
        in node: ProjectNode
    ) -> (ordered: [[SessionEntry]], stopped: [[SessionEntry]]) {
        let pinnedRenderedIDs = Set(pinnedSessions(in: node).map(\.id))
        // Archived rows remain eligible for the fixed inactive preview at the
        // bottom. `pinnedSessions` excludes them because Archive overrides a
        // retained pin until the Session is restored.
        let candidates = node.sessions.filter {
            !pinnedRenderedIDs.contains($0.id)
        }
        // The drag-reorder overlay must survive the recency sort, or a hand-
        // dragged row snaps straight back. Rows absent from the overlay stay
        // newest-first above the hand-ordered block, mirroring
        // `applySessionOrderOverlay`.
        // An in-flight desktop drag previews in memory. Otherwise the shared
        // file wins so a TUI (or any other frontend) drag shows up here; the
        // local overlay remains the fallback for installs that predate it.
        // Date sort ignores every manual-order source (drags are disabled
        // for the group, so no preview can exist either).
        let isRemoteNode = remoteProjectSummariesByID[node.id] != nil
        let dateSorted = isRemoteNode
            ? remoteProjectSummariesByID[node.id]?.dateSorted == true
            : dateSortedProjectIDs.contains(node.id)
        // Remote nodes never consult local shared order files: the Host's
        // bootstrap row order IS the committed order, and an in-flight drag
        // previews on top of it exactly like local.
        let manualOrder = dateSorted
            ? []
            : isRemoteNode
                ? sessionOrderPreviews[node.id]
                    ?? remoteSessionOrderByProject[node.id]
                    ?? []
                : sessionOrderPreviews[node.id]
                    ?? Self.sharedSessionOrder(projectID: node.id)
                    ?? AppDefaults.shared.stringArray(forKey: Self.sessionOrderKey(node.id))
                    ?? []
        var ordered = orderedSessions(candidates, manualOrder: manualOrder)
        if dateSorted {
            ordered = Self.sessionsSortedByRecentActivity(
                ordered,
                restartingSessionIDs: restartingSessionIDs
            )
        }
        let blocks = ordered.map { [$0] }
        var active: [[SessionEntry]] = []
        var stopped: [[SessionEntry]] = []
        var archived: [[SessionEntry]] = []
        for block in blocks {
            if block.contains(where: { sessionIsRecentArchived($0.id) }) {
                archived.append(block)
                continue
            }
            // A restarting (Resume clicked) block is active-in-waiting: it
            // moves to its active-group position immediately instead of
            // sitting in the stopped group until the replacement spawns —
            // restart stabilizes created_at, so this IS its final spot.
            let isActive = block.contains {
                $0.isLive || restartingSessionIDs.contains($0.id)
            }
            if isActive {
                active.append(block)
            } else {
                stopped.append(block)
            }
        }
        let lifecycleRecency: ([SessionEntry]) -> Int64 = { block in
            block.map {
                max(
                    max($0.createdAt, $0.lifecycleAtMs ?? 0),
                    self.sidebarUnpinRecencyBump[$0.id] ?? 0
                )
            }.max() ?? 0
        }
        func newestFirst(
            _ blocks: [[SessionEntry]], recency: ([SessionEntry]) -> Int64
        ) -> [[SessionEntry]] {
            blocks.enumerated().sorted { a, b in
                let ra = recency(a.element)
                let rb = recency(b.element)
                if ra != rb { return ra > rb }
                return a.offset < b.offset
            }.map(\.element)
        }
        let sortedStopped = newestFirst(stopped, recency: lifecycleRecency)
        let sortedArchived = newestFirst(archived) { block in
            block.map { archiveSidebarRecency($0) }.max() ?? 0
        }
        return (
            active + sortedStopped + sortedArchived,
            sortedStopped + sortedArchived
        )
    }

    /// Whether an inactive row must stay in the sidebar past the preview.
    private func inactiveBlockMustStayVisible(_ block: [SessionEntry]) -> Bool {
        block.contains { session in
            session.id == selectedSessionID
                || sessionIsUnread(session.id)
                || sidebarKeepVisibleSessionIDs.contains(session.id)
                || archivingSessionIDs.contains(session.id)
                || removingSessionIDs.contains(session.id)
                || restartingSessionIDs.contains(session.id)
                // Archive-page confirms render on the archive page only; no
                // need to drag the row into the sidebar for them.
                || (confirmingRemoveSessionID == session.id
                    && confirmingRemoveSurface == .sidebar)
                || confirmingArchiveSessionID == session.id
                || editingSessionID == session.id
        }
    }

    /// Memoized per-project sidebar row lists. Every store publish re-runs
    /// each visible ProjectNodeView body, which asked for both lists —
    /// ordering plus a UserDefaults read per project per render pass. The
    /// inputs only change on tree/pin rebuilds and the explicit mutations
    /// that call `invalidateSidebarLists()`.
    private func sidebarLists(
        in node: ProjectNode
    ) -> (pinned: [SessionEntry], displayed: [SessionEntry]) {
        if let cached = sidebarListsCache[node.id] { return cached }
        let pinned = pinnedSessions(in: node)
        let (ordered, inactive) = sidebarSessionBlocks(in: node)
        // A scoped Host already applied its own inactive-preview setting when
        // it built bootstrap. The Controller must render that projection as
        // advertised instead of imposing this Mac's local preference on it.
        let hostAlreadyWindowed = remoteProjectSummariesByID[node.id] != nil
        var visibleInactiveIDs = Set<String>()
        var inactiveRank = 0
        for block in inactive {
            let isInsideInactivePreview = hostAlreadyWindowed
                || inactiveRank < sidebarVisibleSessionLimit
            inactiveRank += 1
            if isInsideInactivePreview || inactiveBlockMustStayVisible(block) {
                visibleInactiveIDs.formUnion(block.map(\.id))
            }
        }
        // Preserve the live-then-inactive section boundary while the inactive
        // projection decides which preview rows fit.
        let displayedBlocks = ordered.filter { block in
            block.contains {
                $0.isLive
                    || restartingSessionIDs.contains($0.id)
                    || visibleInactiveIDs.contains($0.id)
            }
        }
        let rendered = displayedBlocks.flatMap { $0 }
        sidebarListsCache[node.id] = (pinned, rendered)
        return (pinned, rendered)
    }

    func invalidateSidebarLists() {
        sidebarListsCache.removeAll(keepingCapacity: true)
    }

    private func orderedSessions(
        _ sessions: [SessionEntry],
        manualOrder: [String]
    ) -> [SessionEntry] {
        guard !manualOrder.isEmpty else {
            return sessions.sorted { $0.createdAt > $1.createdAt }
        }
        var rank: [String: Int] = [:]
        for (index, id) in manualOrder.enumerated() { rank[id] = index }
        return sessions.sorted { a, b in
            switch (rank[a.id], rank[b.id]) {
            case let (ra?, rb?): return ra < rb
            case (nil, .some): return true
            case (.some, nil): return false
            case (nil, nil): return a.createdAt > b.createdAt
            }
        }
    }

    // MARK: - Pane pre-warming (native-only; no Svelte counterpart)

    /// Sessions whose Ghostty pane should be created and replayed ahead of
    /// selection (mounted hidden by WarmPaneHostView). Fed by sidebar hover
    /// intent and by the first ⌘1–9 targets while ⌘ is held; ordered oldest
    /// first, capped, pruned to live sessions on rescan.
    @Published private(set) var prewarmSessionIDs: [String] = []

    static let prewarmLimit = 3

    /// Request a warm pane for a live, not-currently-shown session.
    func prewarmSession(_ sessionID: String) {
        guard sessionID != selectedSessionID,
              let session = sessionsByID[sessionID], session.isAttachable,
              !removingSessionIDs.contains(sessionID),
              !restartingSessionIDs.contains(sessionID),
              // Project-sidebar members are already mounted in the right
              // panel; a hidden warm mount would double-mount their surface.
              !sessionIsInProjectSidebar(sessionID)
        else { return }
        var ids = prewarmSessionIDs.filter { $0 != sessionID }
        ids.append(sessionID)
        if ids.count > Self.prewarmLimit {
            ids.removeFirst(ids.count - Self.prewarmLimit)
        }
        if ids != prewarmSessionIDs {
            prewarmSessionIDs = ids
        }
    }

    // MARK: - ⌘1–9 session switching (ProjectItem.svelte:502-528, 680-755)

    /// SESSION_SHORTCUT_LIMIT — at most ⌘1…⌘9.
    static let sessionShortcutLimit = 9

    /// True while ⌘ is held and the app is frontmost; visible session rows
    /// of the shortcut project show ⌘N hints in place of the age
    /// (`showCommandShortcuts` in ProjectItem.svelte).
    @Published private(set) var commandHintsVisible = false

    private var shortcutKeyMonitor: Any?
    private var shortcutFlagsMonitor: Any?

    /// The project whose rows answer ⌘1–9: the selected session's project,
    /// else the first top-level project (the Svelte `isActive` project; the
    /// same fallback the titlebar and collapsed "+" use).
    var shortcutProjectID: String? {
        if let session = selectedSession { return session.projectID }
        return displayNodes.first?.project.id
    }

    /// Pinned rows first, then the displayed (truncation-aware) regular
    /// rows, skipping in-flight restart/remove rows, capped at 9 — exactly
    /// `sessionShortcutTargets`. Empty while settings covers the workspace
    /// (sessionShortcutsEnabled gate, App.svelte:1410) or while the project
    /// is collapsed (`showSessionList` gate).
    var sessionShortcutTargets: [SessionEntry] {
        guard !settingsVisible,
              let projectID = shortcutProjectID,
              expandedProjectIDs.contains(projectID),
              let node = findDisplayNode(projectID)
        else { return [] }
        let rows = renderedPinnedSessions(in: node) + renderedDisplayedSessions(in: node)
        let hiddenPaneMemberIDs = activePaneMemberSessionIDs
        return Array(
            rows.filter {
                !restartingSessionIDs.contains($0.id)
                    && !removingSessionIDs.contains($0.id)
                    && !hiddenPaneMemberIDs.contains($0.id)
            }
            .prefix(Self.sessionShortcutLimit)
        )
    }

    /// [session id: 1-based ⌘ index] for one project's rows — empty unless
    /// the hints are showing and the project is the shortcut target, so the
    /// sidebar can render `⌘N` without recomputing targets per row.
    func sessionShortcutHintIndices(forProject projectID: String) -> [String: Int] {
        guard commandHintsVisible, projectID == shortcutProjectID else { return [:] }
        var indices: [String: Int] = [:]
        for (offset, session) in sessionShortcutTargets.enumerated() {
            indices[session.id] = offset + 1
        }
        return indices
    }

    /// Local NSEvent monitors for ⌘1–9 + the held-⌘ hint state. Installed
    /// once by the app delegate on the app's real store — never on the
    /// throwaway stores the snapshot self-tests create.
    func installSessionShortcutMonitors() {
        guard shortcutKeyMonitor == nil else { return }
        shortcutFlagsMonitor = NSEvent.addLocalMonitorForEvents(
            matching: .flagsChanged
        ) { [weak self] event in
            // Local monitors fire on the main thread before dispatch.
            MainActor.assumeIsolated {
                guard let self else { return }
                let flags = event.modifierFlags
                    .intersection(.deviceIndependentFlagsMask)
                let held = flags.contains(.command)
                let targets = self.sessionShortcutTargets
                let visible = held && !targets.isEmpty
                if self.commandHintsVisible != visible {
                    self.commandHintsVisible = visible
                }
                // Holding ⌘ telegraphs a switch: warm the most likely target
                // (just one — surface creation is synchronous main-thread
                // work, and a 3-pane burst caused visible stalls).
                if visible, let first = targets.first {
                    self.prewarmSession(first.id)
                }
                // ⌃ drives the project hints (delayed — ⌃C etc. are constant
                // terminal traffic) and commits an open ⌃Tab switcher on
                // release.
                if flags.contains(.control) {
                    self.scheduleControlHints()
                } else {
                    if self.sessionSwitcher != nil { self.commitSessionSwitcher() }
                    self.setControlHintsVisible(false)
                }
            }
            return event
        }
        shortcutKeyMonitor = NSEvent.addLocalMonitorForEvents(
            matching: .keyDown
        ) { [weak self] event in
            let consumed = MainActor.assumeIsolated { () -> Bool in
                guard let self else { return false }
                if self.handleShortcutKeyDown(event) { return true }
                return false
            }
            return consumed ? nil : event
        }
    }

    /// True when the event was consumed as an app shortcut (⌘K palette,
    /// ⌘T terminal, ⌘1–9 sessions, ⌃1–9 projects, ⌃Tab MRU switcher).
    /// While the palette is open its own monitor owns the list keys; only
    /// ⌘K (close) is handled here.
    private func handleShortcutKeyDown(_ event: NSEvent) -> Bool {
        // Remote Ghostty owns its complete key stream. Local command palette,
        // launch, session, and project shortcuts must neither consume those
        // keys nor mutate the still-loaded Local workspace behind it.
        guard selectedHostScope == .local else { return false }
        let mods = event.modifierFlags
            .intersection([.command, .option, .control, .shift])
        let char = event.charactersIgnoringModifiers?.lowercased()

        // ⌘K/⌘T live in the Session menu for discoverability, but the
        // focused Ghostty surface claims key equivalents that collide with
        // ghostty keybindings before the menu sees them (AppTerminalView.
        // performKeyEquivalent) — so the monitor owns the actual keys.
        if mods == .command, char == "k" {
            toggleCommandPalette()
            return true
        }

        guard !commandPaletteVisible else { return false }

        if mods == .command, char == "t" {
            guard !settingsVisible, editingSessionID == nil else { return false }
            if NSApp.keyWindow?.firstResponder is NSTextView { return false }
            performCommandTAction()
            return true
        }

        // ⌃Tab / ⌃⇧Tab — MRU session switcher (kVK_Tab = 48). Commit
        // happens when ⌃ is released (flags monitor above).
        if event.keyCode == 48, mods == .control || mods == [.control, .shift] {
            guard !settingsVisible, editingSessionID == nil else { return false }
            cycleSessionSwitcher(backward: mods.contains(.shift))
            return true
        }
        // Esc cancels an armed switcher without switching (kVK_Escape = 53).
        if sessionSwitcher != nil, event.keyCode == 53 {
            cancelSessionSwitcher()
            return true
        }

        if mods == .command { return handleSessionShortcutKeyDown(event) }
        if mods == .control { return handleProjectShortcutKeyDown(event) }
        return false
    }

    /// True when the event was consumed as a session shortcut.
    private func handleSessionShortcutKeyDown(_ event: NSEvent) -> Bool {
        // ⌘ alone — ⌥/⌃/⇧ chords pass through untouched
        // (sessionShortcutIndexFromEvent, ProjectItem.svelte:697-701).
        guard event.modifierFlags
            .intersection([.command, .option, .control, .shift]) == .command
        else { return false }
        guard !settingsVisible, editingSessionID == nil else { return false }
        // Digits typed into a focused text field (rename editor uses
        // isEditing above, but settings/sheets may have fields too) keep
        // their normal meaning — the field editor is an NSTextView
        // (shouldIgnoreSessionShortcutTarget parity).
        if NSApp.keyWindow?.firstResponder is NSTextView { return false }
        guard let digit = Self.shortcutDigit(for: event),
              (1...Self.sessionShortcutLimit).contains(digit)
        else { return false }
        let targets = sessionShortcutTargets
        guard digit <= targets.count else { return false }
        selectedSessionID = targets[digit - 1].id
        return true
    }

    /// Physical digit-row and keypad key codes (kVK_ANSI_1…9 / Keypad1…9):
    /// the fallback for layouts where digits are shifted characters (AZERTY)
    /// — the Svelte handler accepts `Digit1-9`/`Numpad1-9` codes the same
    /// way (ProjectItem.svelte:707-713).
    private static let digitKeyCodes: [UInt16: Int] = [
        18: 1, 19: 2, 20: 3, 21: 4, 23: 5, 22: 6, 26: 7, 28: 8, 25: 9,
        83: 1, 84: 2, 85: 3, 86: 4, 87: 5, 88: 6, 89: 7, 91: 8, 92: 9,
    ]

    private static func shortcutDigit(for event: NSEvent) -> Int? {
        if let characters = event.charactersIgnoringModifiers,
           characters.count == 1, let digit = Int(characters) {
            return digit
        }
        return digitKeyCodes[event.keyCode]
    }

    // MARK: - ⌃1–9 project switching

    /// True while ⌃ has been held long enough (see `scheduleControlHints`)
    /// and the app is frontmost; top-level project rows show ⌃N hints.
    @Published private(set) var controlHintsVisible = false
    private var controlHintsWorkItem: DispatchWorkItem?

    /// Top-level (non-folder) sidebar projects answering ⌃1–9, in sidebar
    /// order — the project mirror of `sessionShortcutTargets`.
    var projectShortcutTargets: [ProjectNode] {
        guard selectedHostScope == .local, !settingsVisible else { return [] }
        return Array(
            nodes.filter { $0.project.isFolder != true }
                .prefix(Self.sessionShortcutLimit)
        )
    }

    /// 1-based ⌃ index for one project row — nil unless the hints are
    /// showing and the project is a target.
    func projectShortcutHintIndex(forProject projectID: String) -> Int? {
        guard controlHintsVisible else { return nil }
        return projectShortcutTargets
            .firstIndex { $0.id == projectID }
            .map { $0 + 1 }
    }

    /// ⌃N: expand the project and select its most recently used session
    /// (fallback: first rendered row). A project with no sessions opens the
    /// main-screen launcher instead, so ⌃N is never a dead keystroke.
    func focusProject(_ projectID: String) {
        guard selectedHostScope == .local else { return }
        guard let node = findNode(projectID) else { return }
        expandedProjectIDs.insert(projectID)
        let rows = renderedPinnedSessions(in: node) + renderedDisplayedSessions(in: node)
        let target = sessionMRU.first { id in rows.contains { $0.id == id } }
            ?? rows.first?.id
        if let target {
            revealSessionInSidebar(target)
        } else {
            settingsVisible = false
            archivedProjectID = nil
            selectedSessionID = nil
            launcherProjectID = projectID
        }
    }

    /// True when the event was consumed as a project shortcut (⌃ alone).
    private func handleProjectShortcutKeyDown(_ event: NSEvent) -> Bool {
        guard !settingsVisible, editingSessionID == nil else { return false }
        if NSApp.keyWindow?.firstResponder is NSTextView { return false }
        guard let digit = Self.shortcutDigit(for: event),
              (1...Self.sessionShortcutLimit).contains(digit)
        else { return false }
        let targets = projectShortcutTargets
        guard digit <= targets.count else { return false }
        focusProject(targets[digit - 1].id)
        return true
    }

    /// ⌃ is constant terminal traffic (⌃C, ⌃R, …), so unlike the instant ⌘
    /// hints the ⌃ project hints only appear after the modifier has been
    /// held ~a third of a second on its own.
    private func scheduleControlHints() {
        guard controlHintsWorkItem == nil, !controlHintsVisible else { return }
        let item = DispatchWorkItem { [weak self] in
            MainActor.assumeIsolated {
                guard let self else { return }
                self.controlHintsWorkItem = nil
                guard NSEvent.modifierFlags.contains(.control),
                      self.sessionSwitcher == nil,
                      !self.projectShortcutTargets.isEmpty
                else { return }
                self.controlHintsVisible = true
            }
        }
        controlHintsWorkItem = item
        DispatchQueue.main.asyncAfter(deadline: .now() + 0.35, execute: item)
    }

    private func setControlHintsVisible(_ visible: Bool) {
        controlHintsWorkItem?.cancel()
        controlHintsWorkItem = nil
        if controlHintsVisible != visible { controlHintsVisible = visible }
    }

    // MARK: - ⌃Tab MRU session switcher

    /// Selected-session history, newest first, capped. Feeds the ⌃Tab
    /// switcher and ⌃N's "most recent session in this project". Ids are
    /// validated against `sessionsByID` at read time, so no prune hook is
    /// needed. In-memory only, by design (like the Svelte selection).
    private(set) var sessionMRU: [String] = []
    private static let sessionMRULimit = 24

    private func noteSessionMRU(_ id: String) {
        sessionMRU.removeAll { $0 == id }
        sessionMRU.insert(id, at: 0)
        if sessionMRU.count > Self.sessionMRULimit {
            sessionMRU.removeLast(sessionMRU.count - Self.sessionMRULimit)
        }
    }

    struct SessionSwitcherState: Equatable {
        var sessionIDs: [String]
        var index: Int
    }

    /// Non-nil while ⌃Tab is cycling (⌃ still held). RootView renders the
    /// overlay from this; releasing ⌃ commits, Esc cancels.
    @Published private(set) var sessionSwitcher: SessionSwitcherState?

    /// Sidebar-visible sessions: MRU first, then tree order, capped at 9 —
    /// a switcher longer than that stops being glanceable.
    private var sessionSwitcherCandidates: [String] {
        var treeOrder: [String] = []
        func walk(_ node: ProjectNode) {
            for session in renderedPinnedSessions(in: node)
                + renderedDisplayedSessions(in: node) {
                treeOrder.append(session.id)
            }
            node.worktrees.forEach(walk)
        }
        nodes.forEach(walk)
        let valid = Set(treeOrder)
        var ordered: [String] = []
        var seen = Set<String>()
        for id in sessionMRU where valid.contains(id) && seen.insert(id).inserted {
            ordered.append(id)
        }
        for id in treeOrder where seen.insert(id).inserted {
            ordered.append(id)
        }
        return Array(ordered.prefix(Self.sessionShortcutLimit))
    }

    func cycleSessionSwitcher(backward: Bool) {
        if var state = sessionSwitcher {
            let count = state.sessionIDs.count
            guard count > 0 else { return }
            state.index = (state.index + (backward ? count - 1 : 1)) % count
            sessionSwitcher = state
            prewarmSession(state.sessionIDs[state.index])
        } else {
            let ids = sessionSwitcherCandidates
            guard ids.count > 1 else { return }
            let index = backward ? ids.count - 1 : 1
            sessionSwitcher = SessionSwitcherState(sessionIDs: ids, index: index)
            prewarmSession(ids[index])
        }
    }

    func commitSessionSwitcher() {
        guard let state = sessionSwitcher else { return }
        sessionSwitcher = nil
        guard state.sessionIDs.indices.contains(state.index) else { return }
        revealSessionInSidebar(state.sessionIDs[state.index])
    }

    func cancelSessionSwitcher() {
        sessionSwitcher = nil
    }

    // MARK: - ⌘K command palette

    /// Whether the palette overlay is up. The palette view owns its own
    /// query/selection state and key monitor; the store only hosts
    /// visibility so the menu item and Esc agree.
    @Published var commandPaletteVisible = false

    func toggleCommandPalette() {
        guard selectedHostScope == .local else { return }
        commandPaletteVisible.toggle()
    }

    // MARK: - Launching

    /// Session ▸ New Session (⌘N): launch the leading favorite preset in the
    /// selected session's project, falling back to the first sidebar project.
    /// Prefers an agent favorite over the blank-terminal pseudo-preset so ⌘N
    /// means "new agent session" whenever any agent CLI is favorited.
    func launchDefaultSession() {
        guard selectedHostScope == .local else { return }
        guard let projectID = defaultLaunchProjectID else { return }
        let preset = displayAvailablePresets.first {
            $0.quickLaunch && !$0.command.isEmpty
        } ?? .newTerminal
        launchSession(
            projectID: projectID,
            command: preset.command,
            sourcePresetID: preset.command.isEmpty ? nil : preset.id
        )
    }

    /// Session ▸ New Terminal (⌘T): a plain shell in the same project ⌘N
    /// targets — the blank-terminal pseudo-preset's keyboard path.
    func launchDefaultTerminal() {
        guard selectedHostScope == .local else { return }
        guard let projectID = defaultLaunchProjectID else { return }
        launchSession(projectID: projectID, command: "")
    }

    /// App-wide ⌘T action. The preset-screen branch only changes
    /// presentation; it does not create a Session until the user chooses a
    /// row, so no local Host or manifest is spawned prematurely.
    func performCommandTAction() {
        switch commandTAction {
        case .newTerminal:
            launchDefaultTerminal()
        case .presetPicker:
            guard selectedHostScope == .local,
                  let projectID = defaultLaunchProjectID
            else { return }
            settingsVisible = false
            recentActivityVisible = false
            archivedProjectID = nil
            selectedSessionID = nil
            expandedProjectIDs.insert(projectID)
            launcherProjectID = projectID
        }
    }

    /// The project ⌘N/⌘T launch into: the selected session's project,
    /// falling back to the first sidebar project. The ⌘K palette reads it
    /// too, for its "New session: <preset>" rows.
    var defaultLaunchProjectID: String? {
        let projectID = selectedSessionID.flatMap { displaySessionsByID[$0]?.projectID }
            ?? launcherProjectID
            ?? displayNodes.first(where: { $0.project.isFolder != true })?.id
        guard let projectID, displayProjectsByID[projectID] != nil else { return nil }
        return projectID
    }

    /// Writes a launch file and spawns unpeel-host detached, then polls for
    /// the manifest (≤2s) and selects the new session.
    func launchSession(projectID: String, command: String, sourcePresetID: String? = nil) {
        // Remote scope: session creation is a Host operation. The matching
        // Host preset id is preferred so the Host resolves its own catalog;
        // a bare command travels as-is. No local spawn can happen here.
        // Foreground creates leave Settings immediately, just like the
        // local `spawnSession` path below. The runtime selects the Host-
        // minted row when its refreshed snapshot lands; keeping Settings
        // mounted hid that selection for sibling workspaces and remote
        // Hosts (most visibly after Agents & Apps → Install).
        settingsVisible = false
        let presetID = sourcePresetID
            ?? remotePresetSummaries.first { $0.command == command }?.id
        performRemoteVerb("Couldn't start the session") { runtime in
            try await runtime.createSession(
                projectID: projectID,
                presetID: presetID,
                command: presetID == nil ? command : nil
            )
        }
    }

}

// MARK: - Remote Host scope: display projection and verb plumbing

extension UnpeelStore {
    // MARK: Display accessors (the single seam the views read)

    /// Transitional ownership guard for the still-live Swift Host adapter.
    /// Keep it synchronous: holding this across an `await` could suppress an
    /// unrelated Controller action processed by the main actor.
    func withNativeHostAdapterEffect<T>(_ body: () throws -> T) rethrows -> T {
        nativeHostAdapterEffectDepth += 1
        defer { nativeHostAdapterEffectDepth -= 1 }
        return try body()
    }

    nonisolated static func shouldDisplayHostProjection(
        scope: SelectedHostScope,
        localClientStarted: Bool,
        localProjectionReady: Bool
    ) -> Bool {
        scope != .local || (localClientStarted && localProjectionReady)
    }

    /// Once the Local Host client starts, semantic effects fail closed to its
    /// workspace worker even while startup/recovery is still showing the disk
    /// fallback. A missing socket must never silently reactivate the duplicate
    /// Swift engine. Non-local scopes continue to require a projected entity.
    nonisolated static func shouldRouteHostVerb(
        scope: SelectedHostScope,
        localClientStarted: Bool,
        projectedEntityExists: Bool
    ) -> Bool {
        switch scope {
        case .local:
            return localClientStarted
        case .localWorkspace, .remote:
            return projectedEntityExists
        }
    }

    private func routesSessionVerbThroughHost(_ sessionID: String) -> Bool {
        guard nativeHostAdapterEffectDepth == 0 else { return false }
        return Self.shouldRouteHostVerb(
            scope: selectedHostScope,
            localClientStarted: localHostClientStarted,
            projectedEntityExists: remoteSummary(for: sessionID) != nil
        )
    }

    private func routesProjectVerbThroughHost(_ projectID: String) -> Bool {
        guard nativeHostAdapterEffectDepth == 0 else { return false }
        return Self.shouldRouteHostVerb(
            scope: selectedHostScope,
            localClientStarted: localHostClientStarted,
            projectedEntityExists: remoteProjectSummariesByID[projectID] != nil
        )
    }

    /// True once this instance's Local scope is a client of the workspace
    /// worker: Host snapshots are Local truth and the Swift scan is only the
    /// launch-time fallback. Read-only Local previews (the carousel page)
    /// must then render Host truth as well.
    var localScopeIsHostServed: Bool { localHostClientStarted }

    private var displaysHostProjection: Bool {
        Self.shouldDisplayHostProjection(
            scope: selectedHostScope,
            localClientStarted: localHostClientStarted,
            localProjectionReady: localHostProjectionReady
        )
    }

    private var currentScopeUsesHostControl: Bool {
        selectedHostScope != .local || localHostClientStarted
    }

    private func remoteSummary(for sessionID: String) -> RemoteSessionSummary? {
        remoteSummariesByID[sessionID] ?? remoteArchivedSummaryCache[sessionID]
    }

    /// Name of the selected non-local scope for the green host button,
    /// titlebar, and empty states; nil in Local scope. A local workspace's
    /// registry name is the Controller-local alias and wins over whatever
    /// its bootstrap advertises, matching remote Host rename semantics.
    var remoteScopeDisplayName: String? {
        switch selectedHostScope {
        case .local:
            return nil
        case .localWorkspace(_, let name):
            return name
        case .remote:
            return remoteHostRuntime.snapshot?.macName
                ?? remoteHostStore.selectedDisplayName
                ?? "Remote Host"
        }
    }

    /// Session ids the current Host projection (or local scan) knows about.
    var mcpApprovalKnownSessionIDs: Set<String> {
        Set((displaysHostProjection ? remoteSessionsByID : sessionsByID).keys)
    }

    /// The sidebar tree for the selected Host scope. Identical shape in both
    /// scopes so the views never branch. Pending MCP approvals overlay
    /// attention onto the Session that should show the in-pane prompt.
    var displayNodes: [ProjectNode] {
        McpApprovalAttention.applying(
            to: displaysHostProjection ? remoteNodes : nodes,
            pendingSessionIDs: mcpApprovalAttentionSessionIDs
        )
    }

    var displaySessionsByID: [String: SessionEntry] {
        McpApprovalAttention.applying(
            to: displaysHostProjection ? remoteSessionsByID : sessionsByID,
            pendingSessionIDs: mcpApprovalAttentionSessionIDs
        )
    }

    var displayProjectsByID: [String: Project] {
        displaysHostProjection ? remoteProjectsByID : projectsByID
    }

    /// The directory a terminal pane is seeded with for this Session: the
    /// Session's own launch cwd when the Host published one, else the path
    /// of its project in the SCOPE-AWARE project map. Never the launch-frozen
    /// local `projectsByID`: in a scoped workspace that map does not hold the
    /// Host's projects, which left the pane with no cwd and made cmd-clicked
    /// relative paths silently do nothing. A later OSC 7 report from the
    /// shell still replaces the seed inside the pane.
    func paneWorkingDirectory(for session: SessionEntry) -> String? {
        Self.paneWorkingDirectory(
            sessionCwd: session.cwd,
            projectPath: displayProjectsByID[session.projectID]?.path
        )
    }

    /// Pure seeding rule behind `paneWorkingDirectory(for:)`: a non-empty
    /// Session cwd wins; an absent or blank one falls back to the project
    /// path (older Hosts publish no cwd).
    nonisolated static func paneWorkingDirectory(
        sessionCwd: String?,
        projectPath: String?
    ) -> String? {
        if let cwd = sessionCwd?.trimmingCharacters(in: .whitespacesAndNewlines),
           !cwd.isEmpty {
            return cwd
        }
        return projectPath
    }

    var displayAvailablePresets: [Preset] {
        displaysHostProjection ? remotePresets : availablePresets
    }

    var displayQuickPresetGroups: [QuickPresetGroup] {
        displaysHostProjection ? remoteQuickPresetGroups : quickPresetGroups
    }

    /// Unread badge for one row, whichever Host owns it.
    func sessionIsUnread(_ sessionID: String) -> Bool {
        if let summary = remoteSummary(for: sessionID) { return summary.unread }
        return unreadSessionIDs.contains(sessionID)
    }

    /// An archived row that still renders in the inactive preview (or
    /// because it is pinned); its affordances swap to Restore.
    func sessionIsRecentArchived(_ sessionID: String) -> Bool {
        // Restore & Resume holds the (previously archived) row as a live-in-
        // waiting sidebar row until the Host publishes the replacement.
        if remoteRestartPlaceholders[sessionID] != nil { return false }
        if let summary = remoteSummary(for: sessionID) { return summary.archived }
        return archivedSessionIDs.contains(sessionID)
            && !archivingSessionIDs.contains(sessionID)
    }

    /// The memoized sidebar projection depends on selection only when a local
    /// stopped/archive row may be retained beyond the configured preview.
    /// Kept pure so the hot-path guard stays independently testable.
    nonisolated static func selectionChangeAffectsSidebarLists(
        from oldSessionID: String?,
        to newSessionID: String?,
        windowedInactiveSessionIDs: Set<String>
    ) -> Bool {
        guard oldSessionID != newSessionID else { return false }
        return [oldSessionID, newSessionID].compactMap { $0 }.contains { id in
            windowedInactiveSessionIDs.contains(id)
        }
    }

    /// Archive-library size for the project context menu. Remote projects
    /// carry the Host-computed count; local counts the archived rows.
    func archivedSessionCount(in node: ProjectNode) -> Int {
        if let summary = remoteProjectSummariesByID[node.id] {
            return summary.archivedSessionCount ?? 0
        }
        return archivedSessions(in: node).count
    }

    /// Scope-neutral node lookup for verbs that operate on the displayed tree.
    func findDisplayNode(_ projectID: String) -> ProjectNode? {
        func search(_ nodes: [ProjectNode]) -> ProjectNode? {
            for node in nodes {
                if node.id == projectID { return node }
                if let found = search(node.worktrees) { return found }
            }
            return nil
        }
        return search(displayNodes)
    }

    /// Host-resolved terminal canvas color for a remote row (nil locally).
    func remoteTerminalBackgroundColor(for sessionID: String) -> NSColor? {
        guard let hex = remoteSummariesByID[sessionID]?.terminalBackgroundHex,
              (0 ... 0xFF_FF_FF).contains(hex)
        else { return nil }
        return NSColor(hex: UInt32(hex))
    }

    /// Host-resolved spinner tint for a remote row (nil locally).
    func remoteSpinnerColor(for sessionID: String) -> Color? {
        guard let hex = remoteSummariesByID[sessionID]?.spinnerColorHex,
              (0 ... 0xFF_FF_FF).contains(hex)
        else { return nil }
        return Color(hex: UInt32(hex))
    }

    /// Whether new-session affordances should offer anything for a project.
    /// Remote projects require the Host's `session.create` operation.
    func canCreateSessions(inProject projectID: String) -> Bool {
        if routesProjectVerbThroughHost(projectID) {
            return remoteHostRuntime.supportsHostOperation(
                RemoteHostRuntime.HostOperation.create
            )
        }
        return true
    }

    // MARK: Projection

    /// Rebuild the remote display projection from the runtime's latest
    /// bootstrap. Local truth is untouched: the projection is a parallel,
    /// in-memory view (never persisted, never served to paired phones).
    func projectRemoteScope(snapshot: RemoteBootstrapSnapshot?) {
        let projectsLocalHost = selectedHostScope == .local
            && localHostClientStarted
        guard selectedHostScope != .local || projectsLocalHost else { return }
        guard let snapshot else {
            if projectsLocalHost {
                localHostProjectionReady = false
            }
            // The runtime clears its snapshot only for a fresh selection or
            // a Host switch — never during a same-Host reconnect (the last
            // valid snapshot stays published there). Clear the projection so
            // a different Host's rows can never linger under the new scope.
            if remoteHostRuntime.snapshot == nil {
                clearRemoteScopeProjectionState()
                invalidateSidebarLists()
            }
            return
        }
        if sidebarInteractionBlocksRemoteProjection {
            remoteProjectionDeferredForSidebarInteraction = true
            return
        }
        remoteProjectionDeferredForSidebarInteraction = false

        let previousSummaries = remoteSummariesByID
        let previousProjectSummaries = remoteProjectSummariesByID
        let previousOrder = remoteSessionOrderByProject
        var snapshotProjects = snapshot.projects
        if projectsLocalHost {
            let tombstoned = Set(
                AppDefaults.shared.stringArray(forKey: Self.removedProjectsKey) ?? []
            )
            if !tombstoned.isEmpty {
                let parentByProjectID = snapshotProjects.reduce(
                    into: [String: String]()
                ) { result, summary in
                    if let parentID = summary.parentProjectID {
                        result[summary.id] = parentID
                    }
                }
                let hidden = Self.projectSubtreeIDs(
                    roots: tombstoned,
                    parentByProjectID: parentByProjectID
                )
                snapshotProjects.removeAll { hidden.contains($0.id) }
            }
        }

        var projects: [Project] = []
        var projectSummaries: [String: RemoteProjectSummary] = [:]
        var finalProjectIndex: [String: Int] = [:]
        for (index, summary) in snapshotProjects.enumerated() {
            finalProjectIndex[summary.id] = index
            projectSummaries[summary.id] = summary
        }
        for (index, summary) in snapshotProjects.enumerated() {
            guard finalProjectIndex[summary.id] == index else { continue }
            // Legacy folder membership renders flat, like the local sidebar.
            let parentID = summary.parentProjectID
            projects.append(Project(
                id: summary.id,
                name: summary.name,
                path: summary.path,
                pinnedAt: summary.pinned == true ? 1 : nil,
                parentProjectID: parentID,
                sortOrder: index,
                isFolder: summary.isGroup == true ? true : nil,
                worktreeBranch: summary.worktreeBranch,
                workspacesEnabled: nil,
                mcpBlocked: summary.mcpBlocked ? true : nil
            ))
        }
        let knownProjectIDs = Set(projects.map(\.id))
        var childrenOf: [String: [Project]] = [:]
        var topLevel: [Project] = []
        for project in projects {
            if let parent = project.parentProjectID, knownProjectIDs.contains(parent) {
                childrenOf[parent, default: []].append(project)
            } else {
                topLevel.append(project)
            }
        }
        let childIDsByParent = childrenOf.mapValues { Set($0.map(\.id)) }

        var summaries: [String: RemoteSessionSummary] = [:]
        var entries: [SessionEntry] = []
        var orderByProject: [String: [String]] = [:]
        var finalSessionIndex: [String: Int] = [:]
        for (index, summary) in snapshot.sessions.enumerated() {
            finalSessionIndex[summary.id] = index
            summaries[summary.id] = summary
        }
        for (index, summary) in snapshot.sessions.enumerated() {
            guard finalSessionIndex[summary.id] == index else { continue }
            entries.append(Self.sessionEntry(fromRemote: summary))
            if knownProjectIDs.contains(summary.projectID) {
                orderByProject[summary.projectID, default: []].append(summary.id)
            }
        }

        for summary in snapshotProjects {
            let childIDs = childIDsByParent[summary.id] ?? []
            guard let order = summary.sessionOrder,
                  order.contains(where: { childIDs.contains($0) })
            else { continue }
            orderByProject[summary.id] = order
        }

        // A bootstrap racing a just-finished Session reorder can still carry
        // the old ranks. Hold the dropped relative order until Host truth
        // matches it (or the bounded hold expires), mirroring the project
        // sibling hold below. Folder ids in a mixed order keep their slots.
        let foregroundKey = workspacePoolForegroundKey()
        for projectID in Array(remoteCommittedSessionOrderHolds.keys) {
            guard let hold = remoteCommittedSessionOrderHolds[projectID] else { continue }
            guard hold.workspaceKey == foregroundKey else {
                remoteCommittedSessionOrderHolds.removeValue(forKey: projectID)
                continue
            }
            let memberIDs = Set(snapshot.sessions.compactMap { summary in
                summary.projectID == projectID ? summary.id : nil
            })
            let expected = hold.ids.filter { memberIDs.contains($0) }
            let expectedSet = Set(expected)
            let natural = (orderByProject[projectID] ?? []).filter {
                expectedSet.contains($0)
            }
            if natural == expected || Date().timeIntervalSince(hold.heldAt) > 15 {
                remoteCommittedSessionOrderHolds.removeValue(forKey: projectID)
            } else {
                orderByProject[projectID] = Self.applyingRelativeIDOrder(
                    expected,
                    to: orderByProject[projectID] ?? []
                )
            }
        }

        // Host-routed Resume: hold each source row through the Host's
        // teardown → respawn gap and keep it at the replacement's slot.
        for (sourceID, placeholder) in remoteRestartPlaceholders {
            var resolved = Date().timeIntervalSince(placeholder.startedAt)
                > Self.remoteRestartPlaceholderTimeout
            switch RemoteHostRuntime.replacementSelectionResolution(
                placeholder.intent,
                sessions: snapshot.sessions
            ) {
            case .select, .cancel: resolved = true
            case .wait: break
            }
            if resolved {
                remoteRestartPlaceholders.removeValue(forKey: sourceID)
                restartingSessionIDs.remove(sourceID)
                continue
            }
            let projectID = placeholder.summary.projectID
            guard knownProjectIDs.contains(projectID) else { continue }
            if summaries[sourceID] == nil {
                summaries[sourceID] = placeholder.summary
                entries.append(placeholder.entry)
            }
            var order = orderByProject[projectID] ?? []
            // A mixed (folder-interleaved) order is the shared rank list
            // itself, so a rank it still carries IS the landing slot.
            let mixed = order.contains { projectSummaries[$0] != nil }
            if mixed, order.contains(sourceID) { continue }
            order.removeAll { $0 == sourceID }
            let index = Self.predictedResumeInsertionIndex(
                in: order,
                sourceID: sourceID,
                sourceCreatedAt: placeholder.summary.createdAtUnixMs,
                sharedOrder: placeholder.sharedOrder,
                isRunningRow: { id in
                    guard let row = summaries[id] else { return false }
                    return row.status == .running && !row.archived && !row.pinned
                },
                isSessionRow: { summaries[$0] != nil },
                createdAt: { summaries[$0]?.createdAtUnixMs ?? 0 }
            )
            order.insert(sourceID, at: index)
            orderByProject[projectID] = order
        }

        remoteProjectSummariesByID = projectSummaries
        remoteSummariesByID = summaries
        remoteSessionOrderByProject = orderByProject
        remotePresetSummaries = snapshot.presets

        // Presets: the Host's enabled list, in Host order; stars become the
        // same quick-launch chips the local strip renders.
        let presets = snapshot.presets
            .filter(\.enabled)
            .map { summary in
                Preset(
                    id: summary.id,
                    label: summary.label,
                    command: summary.command,
                    enabled: summary.enabled,
                    quickLaunch: summary.quickLaunch
                )
            }
        remotePresets = presets
        remoteQuickPresetGroups = Self.quickPresetGroups(from: presets)

        // Build the tree with the exact local algorithm: top-level projects,
        // inline child folders (worktrees + groups), sessions in Host order
        // (bootstrap row order IS the Host's committed order).
        var byProject: [String: [SessionEntry]] = [:]
        for entry in entries where knownProjectIDs.contains(entry.projectID) {
            byProject[entry.projectID, default: []].append(entry)
        }
        // A committed-but-unconfirmed reorder holds its order until the
        // snapshot itself carries it (or the hold expires) — drop the hold
        // as soon as the Host's natural order matches, so the Host stays
        // truth for every later change.
        if projectOrderPreview == nil, let hold = remoteCommittedOrderHold {
            let siblings: [Project]
            if let parent = hold.parentID {
                siblings = (childrenOf[parent] ?? [])
                    .sorted { ($0.sortOrder ?? 0) < ($1.sortOrder ?? 0) }
            } else {
                siblings = topLevel.sorted { ($0.sortOrder ?? 0) < ($1.sortOrder ?? 0) }
            }
            let siblingIDs = Set(siblings.map(\.id))
            let natural = siblings.map(\.id).filter { hold.ids.contains($0) }
            let held = hold.ids.filter { siblingIDs.contains($0) }
            if natural == held || Date().timeIntervalSince(hold.heldAt) > 15 {
                remoteCommittedOrderHold = nil
            }
        }
        // An in-flight project drag preview outranks the snapshot order for
        // its sibling set — the same precedence the local overlay path gives
        // `projectOrderPreview` — so remote rows animate live during a drag.
        // Behind it, the committed hold keeps the dropped order stable across
        // stale bootstraps. Never persisted; a confirming bootstrap is truth.
        let orderPreview: (parentID: String?, ids: [String])? =
            projectOrderPreview.map { ($0.parentID, $0.ids) }
                ?? remoteCommittedOrderHold.map { ($0.parentID, $0.ids) }
        func applyOrderPreview(_ base: [Project], parentID: String?) -> [Project] {
            guard let orderPreview, orderPreview.parentID == parentID else { return base }
            var rank: [String: Int] = [:]
            for (index, id) in orderPreview.ids.enumerated() { rank[id] = index }
            let ordered = base.filter { rank[$0.id] != nil }
                .sorted { rank[$0.id]! < rank[$1.id]! }
            guard !ordered.isEmpty else { return base }
            let rest = base.filter { rank[$0.id] == nil }
            return ordered + rest
        }
        func node(for project: Project) -> ProjectNode {
            let childProjects = applyOrderPreview(
                (childrenOf[project.id] ?? [])
                    .filter { $0.worktreeBranch != nil || $0.isFolder == true }
                    .sorted { ($0.sortOrder ?? 0) < ($1.sortOrder ?? 0) },
                parentID: project.id
            )
            return ProjectNode(
                project: project,
                sessions: byProject[project.id] ?? [],
                worktrees: childProjects.map { node(for: $0) }
            )
        }
        let newNodes = applyOrderPreview(
            topLevel.sorted { ($0.sortOrder ?? 0) < ($1.sortOrder ?? 0) },
            parentID: nil
        ).map { node(for: $0) }

        // A malformed/legacy Host must not take the Controller process down
        // with Dictionary(uniqueKeysWithValues:)'s duplicate-key trap. The
        // projection already resolves summary dictionaries last-row-wins;
        // use the same fail-soft rule for display records.
        let projectedProjects = projects.reduce(into: [String: Project]()) {
            $0[$1.id] = $1
        }
        let projectedSessions = entries.reduce(into: [String: SessionEntry]()) {
            $0[$1.id] = $1
        }
        if projectedProjects != remoteProjectsByID {
            remoteProjectsByID = projectedProjects
        }
        if projectedSessions != remoteSessionsByID {
            remoteSessionsByID = projectedSessions
        }
        // Remote App rows color through the same command-keyed table as
        // local ones; their tints came resolved over the wire.
        Self.publishInstalledAppTints(from: entries)
        let affectedProjects = SidebarProjectionChanges.affectedProjects(
            previous: remoteNodes, next: newNodes,
            previousSummaries: previousSummaries, nextSummaries: summaries,
            previousProjects: previousProjectSummaries, nextProjects: projectSummaries,
            previousOrder: previousOrder, nextOrder: orderByProject
        )
        for id in affectedProjects { sidebarListsCache.removeValue(forKey: id) }
        if newNodes != remoteNodes {
            remoteNodes = newNodes
        }
        if selectedHostScope != .local || projectsLocalHost {
            reconcilePaneLayout(eligibleSessionIDs: Set(
                projectedSessions.values.compactMap { session in
                    guard summaries[session.id]?.archived != true,
                          !removingSessionIDs.contains(session.id),
                          session.isAttachable || session.status == .starting
                    else { return nil }
                    return session.id
                }
            ))
        }

        // Entering a non-local Host: swap the expansion set to the Host's own
        // persisted state so open/closed folders are remembered per Host.
        // First visit ever (nothing stored): open every root project once,
        // so selecting a Host never lands on an all-collapsed tree.
        if !projectsLocalHost {
            let hostKey = snapshot.macID
                ?? selectedHostScope.remoteHostID
                ?? selectedHostScope.localWorkspaceHome
                ?? "remote"
            if remoteAutoExpandedHostKey != hostKey {
                remoteAutoExpandedHostKey = hostKey
                remoteRevealedSelectionID = nil
                remoteCommittedOrderHold = nil
                expandedProjectsStorageKey = Self.expandedProjectsKey + "." + hostKey
                if let stored = AppDefaults.shared.stringArray(forKey: expandedProjectsStorageKey) {
                    expandedProjectIDs = Set(stored)
                } else {
                    expandedProjectIDs = Set(newNodes.map(\.id))
                }
            }
        }
        // Keep the selected session's project reachable — but only when the
        // selection changes. The projection also re-runs on every drag-preview
        // hover, and re-expanding a deliberately collapsed project mid-drag
        // pops it open under the cursor.
        if let selected = selectedSessionID,
           selected != remoteRevealedSelectionID,
           let projectID = remoteSessionsByID[selected]?.projectID {
            remoteRevealedSelectionID = selected
            var reveal: Set<String> = [projectID]
            var parent = remoteProjectsByID[projectID]?.parentProjectID
            var hops = 0
            while let current = parent, hops < 16 {
                reveal.insert(current)
                parent = remoteProjectsByID[current]?.parentProjectID
                hops += 1
            }
            if !reveal.isSubset(of: expandedProjectIDs) {
                expandedProjectIDs.formUnion(reveal)
            }
        }

        // Returning to a scope re-selects where you left off (this run),
        // when that Session still exists. Setting it here pushes into the
        // runtime via the selection didSet, so the adopt below agrees.
        if !projectsLocalHost,
           selectedSessionID == nil,
           let remembered = scopeSessionMemory[selectedHostScope.paneScopeID],
           summaries[remembered] != nil {
            selectedSessionID = remembered
        }
        // Adopt the runtime's selection (it owns default selection).
        if !projectsLocalHost,
           selectedSessionID != remoteHostRuntime.selectedSessionID {
            selectedSessionID = remoteHostRuntime.selectedSessionID
        }
        // A removed/archived-away id can linger in the archived cache; prune
        // fetched libraries for projects that vanished from the Host.
        // Every write below publishes to the whole store's observers, and this
        // runs on each 2s Host refresh: only write what actually changed, or
        // an open context menu in the sidebar is torn down every refresh.
        let prunedArchived = remoteArchivedByProject.filter {
            knownProjectIDs.contains($0.key)
        }
        if prunedArchived != remoteArchivedByProject {
            remoteArchivedByProject = prunedArchived
        }
        remoteArchivedSummaryCache.retainProjects(knownProjectIDs)
        if projectsLocalHost {
            saveStartupPresentation(nodes: newNodes, summaries: snapshot.sessions)
            if !localHostProjectionReady {
                localHostProjectionReady = true
            }
            // Provider-created worktree discovery used to ride the disk
            // rescan, which a Host client no longer performs. Keep it alive
            // on the Host refresh instead (5s throttle, git off-main); the
            // off state is handled once, at the toggle, by the purge.
            if showAgentWorktrees {
                _ = syncLinkedWorktreeProjects(from: projects)
            }
            // Phone fits are Host truth on this path (the worker owns
            // /mobile/resize-desktop); the letterbox and its "fit to desktop"
            // control follow the published grid.
            applyHostPublishedPhoneFits(snapshot.sessions)
            if activityLog.refreshFromHost() {
                let entries = activityLog.entries
                if entries != activityLogEntries {
                    activityLogEntries = entries
                }
            }
            // The Local window owns ordinary focus/launcher state. The runtime
            // receives that choice as a mirror, while its correlated create /
            // restart choices arrive through directDataPlaneSelectionIntent.
            // Never pull its last mirrored value back here: a queued snapshot
            // would otherwise dismiss a just-opened New Terminal launcher.
            if let selectedSessionID,
               projectedSessions[selectedSessionID] == nil {
                self.selectedSessionID = nil
            } else if !remoteHostRuntime.directDataPlaneSelectionIntentPending,
                      remoteHostRuntime.selectedSessionID != selectedSessionID {
                remoteHostRuntime.selectDirectDataPlaneSession(selectedSessionID)
            }
        }
        refreshTitlebarBranch()
    }

    func clearRemoteScopeProjection() {
        localHostProjectionReady = false
        clearRemoteScopeProjectionState()
        remoteAutoExpandedHostKey = nil
        remoteRevealedSelectionID = nil
        remoteCommittedOrderHold = nil
        remoteCommittedSessionOrderHolds.removeAll()
        // Back to Local: restore the local expansion set from its own key.
        if expandedProjectsStorageKey != Self.expandedProjectsKey {
            expandedProjectsStorageKey = Self.expandedProjectsKey
            expandedProjectIDs =
                Set(AppDefaults.shared.stringArray(forKey: Self.expandedProjectsKey) ?? [])
        }
        invalidateSidebarLists()
    }

    private func clearRemoteScopeProjectionState() {
        for sourceID in remoteRestartPlaceholders.keys {
            restartingSessionIDs.remove(sourceID)
        }
        remoteRestartPlaceholders = [:]
        remoteNodes = []
        remoteSessionsByID = [:]
        remoteProjectsByID = [:]
        remoteSummariesByID = [:]
        remoteArchivedSummaryCache.removeAll()
        remoteProjectSummariesByID = [:]
        remoteSessionOrderByProject = [:]
        remoteArchivedByProject = [:]
        remotePresetSummaries = []
        remotePresets = []
        remoteQuickPresetGroups = []
    }

    /// Shared remote-summary → display-entry mapping. Also used by the
    /// workspace peek panel to render pooled bootstrap snapshots with the
    /// real sidebar row components.
    /// Feed the command-keyed color table (`Theme.toolColorHex` and friends)
    /// with Host-stamped installed-App tints, so every command-driven surface
    /// — sidebar rows, palette, menu bar, terminal chrome, and the phone wire
    /// — renders Host-resolved App branding without native guessing identity.
    /// Built-in catalog entries still win inside Theme.
    static func publishInstalledAppTints(from entries: [SessionEntry]) {
        var tints: [String: (tint: Int?, spinner: Int?)] = [:]
        for entry in entries {
            guard let app = entry.activeApp else { continue }
            let key = Theme.commandBasename(entry.command)
            guard !key.isEmpty else { continue }
            tints[key] = (app.tintColorHex, app.spinnerTintColorHex)
        }
        Theme.updateInstalledAppTints(tints)
    }

    static func sessionEntry(fromRemote summary: RemoteSessionSummary) -> SessionEntry {
        let status: SessionStatus
        switch summary.status {
        case .exited:
            status = .exited
        case .running:
            switch summary.activity {
            case .starting: status = .starting
            case .working: status = .busy
            case .blocked: status = .attention
            case .done, .idle, .unknown: status = .idle
            }
        }
        var entry = SessionEntry(
            id: summary.id,
            projectID: summary.projectID,
            label: summary.title.isEmpty ? summary.command : summary.title,
            command: summary.command,
            createdAt: summary.createdAtUnixMs,
            ownerPrincipalID: summary.ownerPrincipalID,
            createdByDeviceID: summary.createdByDeviceID,
            sourcePresetID: summary.sourcePresetID,
            status: status,
            activeRuntimeID: summary.activeRuntimeID,
            // Rebuilt from the wire: the remote Host resolved this against
            // ITS central App catalog and PATH, never this Controller's disk.
            activeApp: summary.activeAppID.map { appID in
                HostedAppIdentity(
                    id: appID,
                    name: summary.activeAppName ?? appID,
                    tint: summary.activeAppTintHex.map {
                        String(format: "#%06X", $0)
                    },
                    spinnerTint: nil
                )
            },
            runtimeLaunchPending: summary.runtimeLaunchPending,
            hasResumableState: summary.capabilities?.archive == true,
            lifecycleAtMs: max(
                summary.createdAtUnixMs,
                summary.updatedAtUnixMs ?? 0
            )
        )
        entry.worktreePath = summary.worktreePath
        entry.worktreeBranch = summary.worktreeBranch
        entry.cwd = summary.cwd
        return entry
    }

    /// Quick-strip groups from a flat preset list — same rules as the local
    /// strip: starred presets grouped per CLI, flat-list order.
    private static func quickPresetGroups(from presets: [Preset]) -> [QuickPresetGroup] {
        var groups: [SetupTool: [Preset]] = [:]
        var order: [SetupTool] = []
        for preset in presets where preset.quickLaunch {
            guard let cli = SetupTool.detect(in: preset.command) else { continue }
            if groups[cli] == nil { order.append(cli) }
            groups[cli, default: []].append(preset)
        }
        return order.compactMap { cli in
            guard let presets = groups[cli], !presets.isEmpty else { return nil }
            return QuickPresetGroup(cli: cli, presets: presets)
        }
    }

    // MARK: Remote verbs

    /// Run one user-initiated remote verb; failures surface through the
    /// app's normal error alert with the runtime's failure message. Nothing
    /// is ever retried automatically (an outcome-unknown effect already
    /// triggered a bootstrap refresh inside the runtime).
    func performRemoteVerb(
        _ failureTitle: String,
        onFailure: (@MainActor () -> Void)? = nil,
        _ operation: @escaping @MainActor (RemoteHostRuntime) async throws -> Void
    ) {
        let runtime = remoteHostRuntime
        Task { @MainActor in
            do {
                try await operation(runtime)
            } catch {
                onFailure?()
                let message = (error as? RemoteHostVerbError)?.message
                    ?? error.localizedDescription
                Self.showRemoteVerbFailure(title: failureTitle, message: message)
            }
        }
    }

    private static func showRemoteVerbFailure(title: String, message: String) {
        let alert = NSAlert()
        alert.messageText = title
        alert.informativeText = message
        alert.alertStyle = .warning
        alert.addButton(withTitle: "OK")
        alert.runModal()
    }

    /// Repaint the sidebar after a drag preview changed. Remote nodes only
    /// need the list caches busted (the preview overlay is read at render
    /// time); local rows re-apply the order overlay over the last scan.
    /// Deliberately NOT wrapped in `withAnimation`: the detached drag applies
    /// its preview exactly once at drop, inside a transaction it controls
    /// (unanimated — the rows are already visually in place via the drag's
    /// transform gap, so the layout swap must be invisible).
    func refreshAfterOrderPreviewChange(projectID: String) {
        if routesProjectVerbThroughHost(projectID) {
            invalidateSidebarLists()
            objectWillChange.send()
            return
        }
        rebuildTreeFromLastScan()
        // A folder-only move changes nothing inside `nodes` — the mixed
        // folder-vs-session ranking lives in the shared order, read at
        // render time — so the rebuild above may publish nothing. The
        // preview must render NOW: the detached drag applies it inside the
        // drop's animation-disabled transaction, and a deferred render
        // replays the swap as a visible late move.
        invalidateSidebarLists()
        objectWillChange.send()
    }

    /// Fetch (or refresh) one remote project's archive library.
    func refreshRemoteArchivedSessions(projectID: String) {
        let pageGeneration = remoteArchivePageGeneration
        let hostID = selectedHostScope.remoteHostID
        performRemoteVerb("Couldn't load archived sessions") { [weak self] runtime in
            let sessions = try await runtime.archivedSessions(projectID: projectID)
            guard let self,
                  self.selectedHostScope.remoteHostID == hostID,
                  self.archivedProjectID == projectID,
                  self.remoteArchivePageGeneration == pageGeneration
            else { return }
            self.remoteArchivedSummaryCache.replaceProject(
                projectID,
                summaries: sessions
            )
            self.remoteArchivedByProject[projectID] = sessions.map {
                Self.sessionEntry(fromRemote: $0)
            }
        }
    }
}

// MARK: - Background workspace pool (workspaces-unification phase 7)

extension UnpeelStore {
    /// Deferred pool spin-up (startup perf, 2026-08-18): the pool used to
    /// start during store init — before the first window even painted — so
    /// its first reconcile (gateway child processes, remote dials) competed
    /// with first-frame work. RootView calls this once its layout appears;
    /// the pool starts ~2s later. Idempotent across windows/appearances, and
    /// a store that never hosts a window (preset self-test) never starts the
    /// pool at all.
    func startWorkspacePoolAfterFirstPaint() {
        guard WorkspaceFeature.pickerEnabled,
              !workspacePoolStartScheduled else { return }
        workspacePoolStartScheduled = true
        DispatchQueue.main.asyncAfter(deadline: .now() + 2.0) { [weak self] in
            guard let self else { return }
            self.workspacePool.start(
                targetsProvider: { [weak self] in
                    self?.workspacePoolTargets() ?? []
                },
                excludedKeys: { [weak self] in
                    self?.workspacePoolExcludedKeys() ?? []
                },
                foregroundKey: { [weak self] in
                    self?.workspacePoolForegroundKey()
                }
            )
        }
    }

    /// Every KNOWN workspace except this instance's own Local scope, as pool
    /// targets in the shared user order (so the remote concurrency cap
    /// prefers the workspaces the user ranked first): local registry
    /// workspaces (plus the default workspace when this instance is not it)
    /// over loopback gateways, paired Hosts over their saved Direct
    /// endpoints, and SSH Hosts over their saved transports. Link is
    /// deliberately NOT a pool transport: background polling never opens the
    /// relay downlink — an off-LAN Host simply backs off until reachable.
    func workspacePoolTargets() -> [WorkspacePool.Target] {
        guard WorkspaceFeature.pickerEnabled else { return [] }
        var targets: [WorkspacePool.Target] = []
        let currentHome = Self.currentInstanceNormalizedHome()

        func appendLocal(home: String, name: String) {
            let normalized = UnpeelWorkspaceRegistry.normalizePath(home)
            guard normalized != currentHome else { return }
            let expectedHostID = Self.persistedWorkspaceHostID(home: normalized)
            targets.append(WorkspacePool.Target(
                key: WorkspaceListOrder.localKey(home: normalized),
                name: name,
                transport: .localGateway(
                    unpeelHome: normalized,
                    workspaceName: name,
                    expectedHostID: expectedHostID
                ),
                isRemote: false,
                expectedHostID: expectedHostID,
                fingerprint: "local:\(normalized):\(expectedHostID ?? "-")"
            ))
        }

        if !UnpeelWorkspaceContext.isDefaultInstance {
            appendLocal(
                home: UnpeelWorkspaceRegistry.realUnpeelDir.path,
                name: UnpeelWorkspaceContext.defaultWorkspaceName ?? "Personal"
            )
        }
        for record in UnpeelWorkspaceRegistry.load() {
            appendLocal(home: record.home, name: record.name)
        }
        if RemoteHostFeature.pickerEnabled {
            for host in remoteHostStore.records {
                // Same guards as connecting: a record minted for a different
                // Controller identity or missing credentials is not reachable.
                guard host.controllerDeviceID == remoteHostStore.controllerIdentity.id,
                      let credentials = remoteHostStore.credentials(for: host.hostID)
                else { continue }
                targets.append(WorkspacePool.Target(
                    key: WorkspaceListOrder.pairedKey(hostID: host.hostID),
                    name: host.name,
                    transport: .direct(
                        endpoint: host.endpoint,
                        authToken: credentials.authToken,
                        expectedHostID: host.hostID
                    ),
                    isRemote: true,
                    expectedHostID: host.hostID,
                    fingerprint: "direct:\(host.hostID):\(host.endpoint.absoluteString)"
                ))
            }
            for host in remoteHostStore.sshRecords {
                targets.append(WorkspacePool.Target(
                    key: WorkspaceListOrder.sshKey(id: host.id),
                    name: host.name,
                    transport: .ssh(
                        target: host.target,
                        expectedHostID: host.hostID,
                        mode: host.mode,
                        secret: host.usesStoredSecret
                            ? remoteHostStore.sshSecret(for: host.id)
                            : nil
                    ),
                    isRemote: true,
                    expectedHostID: host.hostID,
                    fingerprint: "ssh:\(host.id):\(host.target):\(host.hostID)"
                ))
            }
        }
        return WorkspaceListOrder.apply(to: targets, key: \.key)
    }

    /// Workspaces the runtime currently serves — the foreground scope or the
    /// warm background connection kept across a return to Local. The pool
    /// must never open a second live connection to them; when the runtime's
    /// connection dies (`selectionConnectionIsActive` false) the pool takes
    /// over polling on the next reconcile.
    func workspacePoolExcludedKeys() -> Set<String> {
        guard remoteHostRuntime.selectionConnectionIsActive else { return [] }
        var keys: Set<String> = []
        for host in remoteHostStore.records
        where remoteHostRuntime.warmConnectionMatches(pinnedHostID: host.hostID) {
            keys.insert(WorkspaceListOrder.pairedKey(hostID: host.hostID))
        }
        for host in remoteHostStore.sshRecords
        where remoteHostRuntime.warmConnectionMatches(pinnedHostID: host.hostID) {
            keys.insert(WorkspaceListOrder.sshKey(id: host.id))
        }
        for home in Self.knownLocalWorkspaceHomes() {
            let pinned = Self.persistedWorkspaceHostID(home: home)
            if remoteHostRuntime.warmConnectionMatches(
                pinnedHostID: pinned,
                workspaceHome: home
            ) {
                keys.insert(WorkspaceListOrder.localKey(home: home))
            }
        }
        return keys
    }

    /// Workspace-order key of the foreground Host projection. The Local
    /// client reports its own key once started so a mirrored worker snapshot
    /// cannot duplicate the native per-session notification pipeline.
    func workspacePoolForegroundKey() -> String? {
        switch selectedHostScope {
        case .local:
            guard localHostClientStarted else { return nil }
            return WorkspaceListOrder.localKey(
                home: Self.currentInstanceNormalizedHome()
            )
        case .localWorkspace(let home, _):
            return WorkspaceListOrder.localKey(home: home)
        case .remote(let hostID):
            if remoteHostStore.records.contains(where: { $0.hostID == hostID }) {
                return WorkspaceListOrder.pairedKey(hostID: hostID)
            }
            return WorkspaceListOrder.sshKey(id: hostID)
        }
    }

    /// The runtime-served workspace (see `workspacePoolExcludedKeys`) with
    /// its display name, for mirroring the runtime's snapshots into the pool.
    func runtimeServedWorkspaceKeyAndName() -> (key: String, name: String)? {
        guard remoteHostRuntime.selectionConnectionIsActive else { return nil }
        for host in remoteHostStore.records
        where remoteHostRuntime.warmConnectionMatches(pinnedHostID: host.hostID) {
            return (WorkspaceListOrder.pairedKey(hostID: host.hostID), host.name)
        }
        for host in remoteHostStore.sshRecords
        where remoteHostRuntime.warmConnectionMatches(pinnedHostID: host.hostID) {
            return (WorkspaceListOrder.sshKey(id: host.id), host.name)
        }
        for (home, name) in Self.knownLocalWorkspaceHomesAndNames() {
            let pinned = Self.persistedWorkspaceHostID(home: home)
            if remoteHostRuntime.warmConnectionMatches(
                pinnedHostID: pinned,
                workspaceHome: home
            ) {
                return (WorkspaceListOrder.localKey(home: home), name)
            }
        }
        return nil
    }

    /// Rescope this window to a pooled workspace from a notification click
    /// (workspace-order key) and select the session that needed input. The
    /// seed/adoption path in the select verbs makes the landing instant.
    func rescopeToPooledWorkspace(key: String, sessionID: String?) {
        guard WorkspaceFeature.pickerEnabled else { return }
        if key.hasPrefix("local:") {
            let home = String(key.dropFirst("local:".count))
            selectLocalWorkspace(home: home, name: workspaceDisplayName(forKey: key))
        } else if key.hasPrefix("host:") {
            selectHost(String(key.dropFirst("host:".count)))
        } else if key.hasPrefix("ssh:") {
            selectHost(String(key.dropFirst("ssh:".count)))
        } else {
            return
        }
        if let sessionID {
            // Lands when the seeded/live snapshot carries the row; otherwise
            // the runtime's default selection already prefers blocked rows.
            remoteHostRuntime.selectSession(sessionID)
        }
    }

    /// Select a row from the global activity dropdown. The workspace key is
    /// part of the identity because different Hosts may legitimately mint the
    /// same Session id. Scope entry adopts the pool's cached bootstrap first,
    /// so the target row is selectable without waiting for a network roundtrip.
    func revealGlobalActivitySession(workspaceKey: String, sessionID: String) {
        let ownLocalKey = WorkspaceListOrder.localKey(
            home: Self.currentInstanceNormalizedHome()
        )
        if workspaceKey == ownLocalKey {
            if selectedHostScope != .local {
                selectHost(nil)
                DispatchQueue.main.async { [weak self] in
                    self?.revealSessionInSidebar(sessionID)
                }
            } else {
                revealSessionInSidebar(sessionID)
            }
            return
        }

        if workspacePoolForegroundKey() == workspaceKey {
            guard remoteSessionsByID[sessionID] != nil else { return }
            closeSettings()
            selectedSessionID = sessionID
            return
        }

        rescopeToPooledWorkspace(key: workspaceKey, sessionID: sessionID)
    }

    /// Display name for a workspace-order key (registry/record names win, the
    /// same alias rule the selector applies).
    func workspaceDisplayName(forKey key: String) -> String {
        if key.hasPrefix("host:") {
            let hostID = String(key.dropFirst("host:".count))
            return remoteHostStore.records
                .first(where: { $0.hostID == hostID })?.name ?? "Workspace"
        }
        if key.hasPrefix("ssh:") {
            let id = String(key.dropFirst("ssh:".count))
            return remoteHostStore.sshRecords
                .first(where: { $0.id == id })?.name ?? "Workspace"
        }
        if key.hasPrefix("local:") {
            let home = String(key.dropFirst("local:".count))
            for (candidate, name) in Self.knownLocalWorkspaceHomesAndNames()
            where candidate == home {
                return name
            }
            return URL(fileURLWithPath: home).lastPathComponent
        }
        return "Workspace"
    }

    private static func currentInstanceNormalizedHome() -> String {
        UnpeelWorkspaceContext.isDefaultInstance
            ? UnpeelWorkspaceRegistry.normalizePath(
                UnpeelWorkspaceRegistry.realUnpeelDir.path
            )
            : UnpeelWorkspaceRegistry.normalizePath(LaunchConfig.unpeelDir.path)
    }

    private static func knownLocalWorkspaceHomes() -> [String] {
        knownLocalWorkspaceHomesAndNames().map(\.0)
    }

    private static func knownLocalWorkspaceHomesAndNames() -> [(String, String)] {
        var homes: [(String, String)] = [(
            UnpeelWorkspaceRegistry.normalizePath(
                UnpeelWorkspaceRegistry.realUnpeelDir.path
            ),
            UnpeelWorkspaceContext.defaultWorkspaceName ?? "Personal"
        )]
        for record in UnpeelWorkspaceRegistry.load() {
            homes.append((
                UnpeelWorkspaceRegistry.normalizePath(record.home),
                record.name
            ))
        }
        return homes
    }

    private static func persistedWorkspaceHostID(home: String) -> String? {
        MobilePairingStore.persistedHostID(
            at: URL(fileURLWithPath: home, isDirectory: true)
                .appendingPathComponent("mobile")
                .appendingPathComponent("mac-id")
        )
    }
}

// MARK: - Off-main rescan disk snapshot

/// The per-session-dir disk inputs one rescan consumes. Scheduled rescans
/// collect this on a background queue so the main thread never pays the
/// scan's filesystem pass — with a streaming session, rescans fire 1-2×/s
/// forever (busy sweep + manifest churn), and each on-main pass was a
/// 20-60ms main-queue stall exactly where ghostty's frame presents land.
struct ScanDiskSnapshot {
    /// Session dir names with a decodable manifest, in directory order.
    var dirNames: [String] = []
    /// dirName → decoded manifest.
    var manifests: [String: HostedSessionManifest] = [:]
    /// dirName → output.bin size, for running manifests only.
    var outputSignals: [String: UInt64] = [:]
    /// dirNames whose archived marker file exists.
    var archivedMarkerDirs: Set<String> = []
    /// Dirs whose managed runtime has produced a provider conversation or
    /// provider-owned storage. This is the effective Archive/Resume gate.
    var resumableStateDirs: Set<String> = []
    /// Purge-tombstoned dirs whose TTL expired during collection; the main
    /// thread drops them from `purgedSessionDirs`.
    var expiredPurgedDirs: [String] = []
    /// Dirs whose manifest matched the decode cache byte-for-byte — the
    /// derivation phase can reuse the previous entry for exited sessions.
    var unchangedManifestDirs: Set<String> = []
    /// dirName → project-override.json stamp (absent = no marker). Entry
    /// reuse requires the override marker to be unchanged too.
    fileprivate var projectOverrideStamps: [String: UnpeelStore.FileStamp] = [:]
}

/// Owns the manifest decode cache and produces `ScanDiskSnapshot`s. Safe to
/// call from any thread — the cache is lock-guarded, and the main thread's
/// synchronous `rescan()` path and the background collection queue share it.
final class ScanSnapshotCollector: @unchecked Sendable {
    private let lock = NSLock()
    /// manifest.json decode cache keyed by session dir name. `manifest` is
    /// nil when the last read failed to decode (torn write); a finished
    /// write changes mtime/size and re-triggers the decode.
    private var manifestCache:
        [String: (stamp: UnpeelStore.FileStamp, manifest: HostedSessionManifest?)] = [:]

    func removeCachedManifest(_ dirName: String) {
        lock.lock()
        manifestCache.removeValue(forKey: dirName)
        lock.unlock()
    }

    /// Drop cache entries for dirs that vanished (end-of-scan GC).
    func retainCachedManifests(_ dirNames: Set<String>) {
        lock.lock()
        manifestCache = manifestCache.filter { dirNames.contains($0.key) }
        lock.unlock()
    }

    /// One filesystem pass over the sessions root. `purged` mirrors the
    /// store's purge tombstones: dirs inside their TTL are deleted (the old
    /// host may have resurrected them) and excluded; expired ones are
    /// reported back for tombstone cleanup.
    func collect(
        root: String,
        purged: [String: Date],
        purgedTTL: TimeInterval,
        now: Date
    ) -> ScanDiskSnapshot {
        var snapshot = ScanDiskSnapshot()
        let names = (try? FileManager.default.contentsOfDirectory(atPath: root)) ?? []
        lock.lock()
        defer { lock.unlock() }
        for dirName in names where !dirName.hasPrefix(".") {
            let dirPath = root + "/" + dirName
            let manifestPath = dirPath + "/manifest.json"

            if let purgedAt = purged[dirName] {
                if now.timeIntervalSince(purgedAt) < purgedTTL {
                    try? FileManager.default.removeItem(atPath: dirPath)
                    manifestCache.removeValue(forKey: dirName)
                    continue
                }
                snapshot.expiredPurgedDirs.append(dirName)
            }

            guard let stamp = UnpeelStore.statFile(manifestPath) else {
                manifestCache.removeValue(forKey: dirName)
                continue
            }
            let manifest: HostedSessionManifest
            if let cached = manifestCache[dirName], cached.stamp == stamp {
                guard let cachedManifest = cached.manifest else { continue }
                manifest = cachedManifest
                snapshot.unchangedManifestDirs.insert(dirName)
            } else {
                let decoded = (try? Data(contentsOf: URL(fileURLWithPath: manifestPath)))
                    .flatMap { try? JSONDecoder().decode(HostedSessionManifest.self, from: $0) }
                manifestCache[dirName] = (stamp, decoded)
                guard let decoded else { continue }
                manifest = decoded
            }

            snapshot.dirNames.append(dirName)
            snapshot.manifests[dirName] = manifest
            if manifest.state == "running",
               let outputStamp = UnpeelStore.statFile(dirPath + "/output.bin") {
                snapshot.outputSignals[dirName] = UInt64(clamping: outputStamp.size)
            }
            if access(
                dirPath + "/" + UnpeelStore.SharedMarker.archived.rawValue, F_OK
            ) == 0 {
                snapshot.archivedMarkerDirs.insert(dirName)
            }
            if UnpeelStore.hasDurableResumeEvidence(
                manifest: manifest,
                dirPath: dirPath
            ) {
                snapshot.resumableStateDirs.insert(dirName)
            }
            if let overrideStamp = UnpeelStore.statFile(
                dirPath + "/project-override.json"
            ) {
                snapshot.projectOverrideStamps[dirName] = overrideStamp
            }
        }
        return snapshot
    }
}
