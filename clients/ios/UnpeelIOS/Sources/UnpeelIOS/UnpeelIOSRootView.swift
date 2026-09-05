import SwiftUI
import UIKit
import UnpeelShared

public struct UnpeelIOSRootView: View {
    @State private var store: RemotePreviewStore
    @StateObject private var connection = RemoteConnectionStore()
    @Environment(\.horizontalSizeClass) private var horizontalSizeClass
    @Environment(\.scenePhase) private var scenePhase
    @State private var refreshLoopTask: Task<Void, Never>?
    @State private var refreshWatchdogTask: Task<Void, Never>?
    private let appLock = AppLockManager.shared

    public init(store: RemotePreviewStore = RemotePreviewStore()) {
        _store = State(initialValue: store)
    }

    public var body: some View {
        ZStack {
            content

            if horizontalSizeClass != .regular {
                SessionsDrawerOverlay(store: store)
                    .zIndex(20)
            }

            PresetDrawerOverlay(store: store)
                .zIndex(30)
            // App lock cover — above everything, including the drawers. Root
            // sheets are dismissed when the lock engages (below), so nothing
            // can present over it.
            if appLock.isLocked {
                AppLockOverlayView(lock: appLock)
                    .zIndex(100)
                    .transition(.opacity)
            }
        }
        .animation(.easeOut(duration: 0.18), value: appLock.isLocked)
        .environmentObject(connection)
        .sheet(isPresented: $connection.pairingSheetPresented) {
            PairingView(connection: connection, store: store)
        }
        // Bell (activity) + organize sheets live at the ROOT — the same level
        // as pairing, which is the only place a `.sheet` presents reliably
        // over the Metal terminal surface.
        .sheet(item: Binding(
            get: { store.topBarSheet },
            set: { store.topBarSheet = $0 }
        )) { sheet in
            switch sheet {
            case .activity:
                ActivitySessionsPanel(
                    blocked: store.bellBlockedSessions,
                    active: store.bellActiveSessions,
                    recent: store.bellRecentSessions,
                    projectsByID: store.projectsByID
                ) { selected in
                    store.topBarSheet = nil
                    store.select(selected)
                }
                .presentationDetents([.medium])
                .presentationDragIndicator(.visible)
            case .organize:
                if let session = store.organizeSheetSession {
                    SessionOrganizeSheet(store: store, session: session)
                        .presentationDetents([.medium])
                        .presentationDragIndicator(.visible)
                }
            case .organizeProject:
                if let project = store.organizeSheetProject {
                    ProjectOrganizeSheet(store: store, project: project)
                        .presentationDetents([.medium])
                        .presentationDragIndicator(.visible)
                }
            case .gallery:
                if let session = store.selectedSession {
                    BrowserGalleryPanel(
                        client: store.client,
                        sessionID: session.id,
                        supportsResumableUpload: store.supportsResumableArtifactUpload,
                        onApply: { data, contentType in
                            store.attachImageToComposer?(data, contentType)
                            store.topBarSheet = nil
                        }
                    )
                    .presentationDragIndicator(.visible)
                }
            case .textSelection:
                if let payload = store.terminalTextSelection {
                    TerminalTextSelectionSheet(payload: payload) {
                        store.topBarSheet = nil
                    }
                    .presentationDetents([.medium, .large])
                    .presentationDragIndicator(.visible)
                }
            case .archive:
                if let projectID = store.archiveLibraryProjectID,
                   let project = store.projectsByID[projectID] {
                    ArchivedSessionsSheet(store: store, project: project)
                        .presentationDetents([.medium, .large])
                        .presentationDragIndicator(.visible)
                }
            }
        }
        .onChange(of: store.topBarSheet != nil) { presented in
            if presented { dismissTerminalKeyboard() }
        }
        .onChange(of: connection.pairingSheetPresented) { presented in
            if presented { dismissTerminalKeyboard() }
        }
        .onAppear {
            // A background/locked cold launch may have preserved pairing
            // records while their WhenUnlocked Keychain items were hidden.
            // Rehydrate before exposing pairing state or adopting the client.
            connection.retryKeychainHydrationIfNeeded()
            store.adoptClient(
                connection.client,
                connectionEpoch: connection.epoch
            )
            // Lost Direct connections recover through the E2E Relay. Bonjour
            // remains an unauthenticated discovery hint and never receives the
            // saved bearer. Once Relay authenticates the paired Host, its
            // bootstrap may safely repair this phone's stale Direct IP/port.
            store.attemptRelayFallback = { [weak connection, weak store] in
                guard let connection, let store else { return false }
                let recovered = await connection.activateRelayFallback()
                if recovered {
                    store.adoptClient(
                        connection.client,
                        connectionEpoch: connection.epoch
                    )
                }
                return recovered
            }
            store.attemptDirectRestore = { [weak connection, weak store] in
                guard let connection, let store else { return false }
                let restored = await connection.restoreDirectConnection()
                if restored {
                    store.adoptClient(
                        connection.client,
                        connectionEpoch: connection.epoch
                    )
                }
                return restored
            }
            store.onDirectPollSucceeded = { [weak connection, weak store] proof in
                guard let connection, let store else { return }
                // A Host that serves TLS on /mobile moves this connection to
                // pinned HTTPS first; the credential refresh below belongs to
                // the generation that answered, so it is skipped on a switch.
                if connection.adoptDirectTransport(after: proof) {
                    store.adoptClient(
                        connection.client,
                        connectionEpoch: connection.epoch
                    )
                    return
                }
                await connection.ensureRelayCredentials(after: proof)
            }
            store.onRelayPollSucceeded = { [weak connection, weak store] proof in
                guard let connection, let store else { return false }
                // Over the relay this only persists the (authenticated) TLS
                // decision so the next Direct probe is already pinned.
                connection.adoptDirectTransport(after: proof)
                let refreshed = await connection.refreshDirectEndpoint(after: proof)
                if refreshed {
                    store.adoptClient(
                        connection.client,
                        connectionEpoch: connection.epoch
                    )
                }
                return refreshed
            }
            store.onDirectPlaintextRefused = { [weak connection, weak store] epoch in
                guard let connection, let store else { return false }
                let upgraded = connection.upgradeDirectTransportAfterPlaintextRefusal(
                    connectionEpoch: epoch
                )
                if upgraded {
                    store.adoptClient(
                        connection.client,
                        connectionEpoch: connection.epoch
                    )
                }
                return upgraded
            }
            // A device build with no paired Mac has nothing to talk to —
            // land straight in pairing instead of an empty preview.
            if connection.needsPairing {
                connection.pairingSheetPresented = true
            }
            // Push: upload the APNs token to EVERY paired Mac whenever it
            // (re)arrives — notifications must work from non-active Macs
            // too — and route a tapped notification to its session.
            PushManager.shared.onTokenChange = { [weak connection] token, environment in
                connection?.registerPushTokenEverywhere(
                    apnsToken: token, environment: environment
                )
            }
            PushManager.shared.onOpenSession = { [weak store] sessionID in
                store?.selectSessionByID(sessionID)
            }
            PushManager.shared.uploadCachedToken()
        }
        .onChange(of: connection.epoch) { _ in
            store.adoptClient(
                connection.client,
                connectionEpoch: connection.epoch
            )
            Task { await store.loadFromBridge() }
            // New/re-paired Mac client — (re)register the push token with it.
            PushManager.shared.uploadCachedToken()
        }
        // Fold the served workspace's advertised name and tint into its
        // stored pairing record so the Workspaces list stays fresh for
        // non-active entries too.
        .onChange(of: store.hostTintHue) { hue in
            connection.noteActiveHostIdentity(
                name: store.snapshot.macName, tintHue: hue
            )
        }
        .onChange(of: store.snapshot.macName) { name in
            connection.noteActiveHostIdentity(
                name: name, tintHue: store.hostTintHue
            )
        }
        .task {
            startRefreshLoop()
        }
        .onChange(of: scenePhase) { phase in
            switch phase {
            case .background:
                // Polling while backgrounded just queues a request iOS will
                // suspend mid-flight; its failure on unlock used to flash the
                // disconnected state over a perfectly healthy connection.
                stopRefreshLoop()
                appLock.lockIfEnabled()
                if appLock.isLocked {
                    // Sheets present over the ZStack, so they'd float above
                    // the lock cover — drop them while locked. The keyboard
                    // would too.
                    store.topBarSheet = nil
                    connection.pairingSheetPresented = false
                    dismissTerminalKeyboard()
                }
            case .active:
                connection.retryKeychainHydrationIfNeeded()
                if connection.needsPairing {
                    connection.pairingSheetPresented = true
                }
                // Restart immediately (the loop's first step is a bootstrap
                // fetch) so the first visible frame paints from fresh data.
                startRefreshLoop(afterResume: true)
                // One automatic biometric prompt per foreground (covers cold
                // launch too); the overlay's button handles retries.
                Task { await appLock.autoUnlockOnForeground() }
            default:
                break
            }
        }
        .onReceive(NotificationCenter.default.publisher(
            for: UIApplication.protectedDataDidBecomeAvailableNotification
        )) { _ in
            connection.retryKeychainHydrationIfNeeded()
            if connection.needsPairing {
                connection.pairingSheetPresented = true
            }
        }
        .onDisappear {
            stopRefreshLoop()
        }
    }

    private func startRefreshLoop(afterResume: Bool = false) {
        guard refreshLoopTask == nil else { return }
        if afterResume {
            store.prepareForForegroundResume()
        }
        refreshLoopTask = Task {
            await store.runBridgeRefreshLoop()
        }
        startRefreshWatchdog()
    }

    private func stopRefreshLoop() {
        refreshLoopTask?.cancel()
        refreshLoopTask = nil
        refreshWatchdogTask?.cancel()
        refreshWatchdogTask = nil
    }

    /// The refresh loop is strictly sequential, so one poll hanging mid-await
    /// (a half-open connection across a Mac restart survives every request
    /// timeout miss) freezes the sidebar at the last applied snapshot while
    /// the banner keeps saying "Connected" — task cancellation can't help
    /// because the await never returns to observe it. Watch the store's
    /// poll-completion heartbeat instead and replace the whole loop task when
    /// it stops: the wedged task is abandoned (it dies with its connection),
    /// and the fresh one repolls within 2s.
    private func startRefreshWatchdog() {
        guard refreshWatchdogTask == nil else { return }
        refreshWatchdogTask = Task {
            while !Task.isCancelled {
                // Backgrounding cancels this task (stopRefreshLoop), so
                // reaching here means the app is foregrounded — or just woke
                // from suspension with a stale heartbeat, where a restart is
                // exactly right anyway.
                try? await Task.sleep(nanoseconds: 6_000_000_000)
                guard !Task.isCancelled else { break }
                if Date().timeIntervalSince(store.lastPollCompletedAt) > 20 {
                    RefreshDiagnostics.log("watchdog: poll heartbeat stale — restarting refresh loop")
                    refreshLoopTask?.cancel()
                    store.prepareForForegroundResume()
                    refreshLoopTask = Task {
                        await store.runBridgeRefreshLoop()
                    }
                }
            }
        }
    }

    @ViewBuilder
    private var content: some View {
        if horizontalSizeClass == .regular {
            NavigationSplitView {
                SessionSidebarView(store: store)
            } detail: {
                terminalDetail
            }
        } else {
            terminalDetail
        }
    }

    /// The rounded Surface card owns the full available width; title controls
    /// remain outside it on the lighter frame.
    private var terminalDetail: some View {
        TerminalDetailView(store: store)
    }
}

/// Shared drawer geometry for tap-open and interactive edge drags.
enum IOSSidebarRevealLayout {
    static func drawerWidth(for size: CGSize, regular: Bool) -> CGFloat {
        if regular {
            return min(380, max(320, size.width * 0.40))
        }
        return min(360, max(286, size.width * 0.88))
    }

    static func revealDistance(
        presented: Bool,
        interactiveReveal: CGFloat?,
        drawerWidth: CGFloat
    ) -> CGFloat {
        guard drawerWidth > 0 else { return 0 }
        if let interactiveReveal {
            return min(drawerWidth, max(0, interactiveReveal))
        }
        return presented ? drawerWidth : 0
    }
}

private struct SessionsDrawerOverlay: View {
    var store: RemotePreviewStore
    @Environment(\.horizontalSizeClass) private var horizontalSizeClass
    /// True while the current gesture is a horizontal drawer slide — the list
    /// scroll is locked out for its duration so a slide never scrolls the
    /// sidebar too (one direction at a time).
    @State private var horizontalSlideLock = false
    /// What the current drag IS — decided ONCE, from its initial direction.
    /// The previous per-change `|w| > |h|` re-test let an ordinary vertical
    /// list scroll grab the drawer whenever its start (or a mid-scroll
    /// drift) was momentarily width-dominant: the panel lurched sideways
    /// and the list froze — the drawer's phantom "horizontal scrolling".
    private enum CloseDragIntent { case slide, scroll }
    @State private var closeDragIntent: CloseDragIntent?
    /// Mirrors the drag's active phase. A cancelled gesture never reaches
    /// `.onEnded`, which would strand the slide lock and a partial reveal
    /// (content stuck shifted right, list scroll dead); the
    /// @GestureState reset is the one end-of-gesture signal SwiftUI
    /// guarantees, so cleanup also keys off it going false.
    @GestureState private var closeDragActive = false

    var body: some View {
        GeometryReader { geometry in
            let drawerWidth = IOSSidebarRevealLayout.drawerWidth(
                for: geometry.size,
                regular: horizontalSizeClass == .regular
            )
            let visibleWidth = IOSSidebarRevealLayout.revealDistance(
                presented: store.sessionsDrawerPresented,
                interactiveReveal: store.sidebarDragReveal,
                drawerWidth: drawerWidth
            )
            let drawerX = visibleWidth - drawerWidth
            let openFraction = drawerWidth > 0 ? visibleWidth / drawerWidth : 0
            let mounted = store.sessionsDrawerPresented || store.sidebarDragReveal != nil

            ZStack(alignment: .leading) {
                if mounted {
                    Color.black.opacity(0.38 * openFraction)
                        .ignoresSafeArea()
                        .transition(.opacity)
                        .onTapGesture { store.hideSessions() }

                    drawerContent(topInset: geometry.safeAreaInsets.top)
                        .frame(width: drawerWidth)
                        .frame(maxHeight: .infinity)
                        .background(IOSSidebarTheme.background(tintHue: store.hostTintHue))
                        .overlay(alignment: .trailing) {
                            Rectangle()
                                .fill(.white.opacity(0.10))
                                .frame(width: 1)
                        }
                        .clipShape(UnevenRoundedRectangle(
                            topLeadingRadius: 0,
                            bottomLeadingRadius: 0,
                            bottomTrailingRadius: 16,
                            topTrailingRadius: 16,
                            style: .continuous
                        ))
                        .compositingGroup()
                        .shadow(color: .black.opacity(0.40), radius: 22, x: 8, y: 0)
                        .offset(x: drawerX)
                        .simultaneousGesture(closeDrag(drawerWidth: drawerWidth))
                        .transition(.move(edge: .leading).combined(with: .opacity))
                }
            }
            .frame(width: geometry.size.width, height: geometry.size.height)
            // ONLY hit-testable when actually presented. During an open-drag the
            // finger is already captured by the terminal's gesture; letting the
            // overlay grab touches while a peek is active is what froze the app
            // when a peek got stuck (the overlay swallowed every touch).
            .allowsHitTesting(store.sessionsDrawerPresented)
        }
        .ignoresSafeArea()
        .animation(
            .timingCurve(0.16, 1, 0.3, 1, duration: 0.28),
            value: store.sessionsDrawerPresented
        )
        // The drawer slides over the terminal; a lingering keyboard both
        // covers the session list and keeps typing routed at the terminal.
        .onChange(of: store.sessionsDrawerPresented) { presented in
            if presented {
                dismissTerminalKeyboard()
            }
        }
        // Cleanup for CANCELLED close-drags (`.onEnded` never runs for
        // those): when the @GestureState mirror resets, release the scroll
        // lock and spring a partial slide home instead of leaving the panel
        // stranded mid-offset. After a normal end this is a no-op.
        .onChange(of: closeDragActive) { active in
            guard !active else { return }
            closeDragIntent = nil
            horizontalSlideLock = false
            if store.sessionsDrawerPresented, store.sidebarDragReveal != nil {
                withAnimation(.spring(response: 0.32, dampingFraction: 0.86)) {
                    store.sidebarDragReveal = nil
                }
            }
        }
        .onChange(of: store.presetDrawerProjectID) { projectID in
            if projectID != nil { dismissTerminalKeyboard() }
        }
    }

    /// Swipe the drawer left to dismiss. `.simultaneousGesture` so the session
    /// list still scrolls vertically; the first callback past the minimum
    /// distance decides ONCE whether this gesture is a drawer slide (clearly
    /// horizontal start) or a list scroll (anything else) — a scroll can then
    /// never translate the drawer, however sideways it later drifts.
    /// Measured in `.global` space because the drawer itself is translated;
    /// a local read would feed that movement back into the drag.
    private func closeDrag(drawerWidth: CGFloat) -> some Gesture {
        DragGesture(minimumDistance: 12, coordinateSpace: .global)
            .updating($closeDragActive) { _, state, _ in state = true }
            .onChanged { value in
                let intent = closeDragIntent ?? {
                    let intent: CloseDragIntent =
                        abs(value.translation.width) > abs(value.translation.height)
                        ? .slide : .scroll
                    closeDragIntent = intent
                    if intent == .slide {
                        // Lock the list scroll for the slide's duration so
                        // the two never fight (one direction at a time).
                        horizontalSlideLock = true
                    }
                    return intent
                }()
                guard intent == .slide else { return }
                // Interactive visible width: full drawer plus the leftward
                // translation. The terminal below remains stationary.
                store.sidebarDragReveal = min(
                    drawerWidth,
                    max(0, drawerWidth + min(0, value.translation.width))
                )
            }
            .onEnded { value in
                let intent = closeDragIntent
                closeDragIntent = nil
                horizontalSlideLock = false
                guard intent == .slide else { return }
                let dismiss = value.translation.width < -80
                    || value.predictedEndTranslation.width < -180
                if dismiss {
                    withAnimation(.timingCurve(0.16, 1, 0.3, 1, duration: 0.22)) {
                        store.sessionsDrawerPresented = false
                        store.sidebarDragReveal = nil
                    }
                } else {
                    withAnimation(.spring(response: 0.32, dampingFraction: 0.86)) {
                        store.sidebarDragReveal = nil
                    }
                }
            }
    }

    @ViewBuilder
    private func drawerContent(topInset: CGFloat) -> some View {
        SessionSidebarView(
            store: store,
            topContentInset: topInset,
            scrollLocked: horizontalSlideLock
        )
    }

}

private struct PresetDrawerOverlay: View {
    var store: RemotePreviewStore

    /// Live drag offset while the user is swiping the sheet down to dismiss.
    @State private var dragOffset: CGFloat = 0

    /// Row height (`PresetDrawerRow` pins `minHeight: 52`) and inter-row spacing,
    /// used to size the list deterministically — no runtime GeometryReader
    /// measurement, which otherwise re-fires every drag frame and makes the
    /// dismiss drag stutter.
    private static let rowHeight: CGFloat = 52
    private static let rowSpacing: CGFloat = 7

    private var project: RemoteProjectSummary? {
        store.presetDrawerProject
    }

    private var presets: [RemotePresetSummary] {
        // Keep the snapshot's order: the Mac sends presets in the same order
        // the desktop "+" menu shows them, so the two menus stay identical.
        store.snapshot.presets
            .filter { $0.enabled && $0.supportsIOSSessionAPI }
    }

    var body: some View {
        GeometryReader { geometry in
            ZStack(alignment: .bottom) {
                if let project {
                    Color.black.opacity(0.30)
                        .ignoresSafeArea()
                        .onTapGesture {
                            store.hidePresetDrawer()
                        }
                        .transition(.opacity)

                    // Cap the list so the drawer can never grow past the screen;
                    // when the presets overflow this, the ScrollView scrolls.
                    let maxListHeight = max(
                        200,
                        geometry.size.height - geometry.safeAreaInsets.top - 150
                    )
                    let listBottomPadding = max(16, geometry.safeAreaInsets.bottom + 10)
                    let naturalListHeight = CGFloat(presets.count) * Self.rowHeight
                        + CGFloat(max(0, presets.count - 1)) * Self.rowSpacing
                        + listBottomPadding
                    let listHeight = min(naturalListHeight, maxListHeight)

                    VStack(spacing: 0) {
                        // Header (drag handle + title + close) — carries the
                        // swipe-down-to-dismiss gesture. Kept out of the
                        // ScrollView so the gesture never fights scrolling.
                        VStack(spacing: 0) {
                            Capsule()
                                .fill(Color.white.opacity(0.26))
                                .frame(width: 42, height: 4)
                                .padding(.top, 10)
                                .padding(.bottom, 12)

                            HStack(spacing: 10) {
                                VStack(alignment: .leading, spacing: 2) {
                                    Text("New session")
                                        .font(.system(size: 17, weight: .semibold))
                                        .foregroundStyle(IOSSidebarTheme.foreground)
                                    Text(project.name)
                                        .font(.system(size: 12, weight: .medium))
                                        .foregroundStyle(IOSSidebarTheme.mutedForeground)
                                        .lineLimit(1)
                                }

                                Spacer(minLength: 0)

                                Button {
                                    store.hidePresetDrawer()
                                } label: {
                                    Image(systemName: "xmark")
                                        .font(.system(size: 13, weight: .semibold))
                                        .frame(width: 34, height: 34)
                                }
                                .foregroundStyle(IOSSidebarTheme.mutedForeground)
                                .iosGlassControl(cornerRadius: 11)
                                .accessibilityLabel("Close presets")
                            }
                            .padding(.horizontal, 18)
                            .padding(.bottom, 10)
                        }
                        .contentShape(Rectangle())
                        .gesture(dismissDrag)

                        ScrollView {
                            VStack(spacing: 7) {
                                ForEach(presets) { preset in
                                    PresetDrawerRow(
                                        preset: preset,
                                        launching: store.launchingPresetID == preset.id
                                    ) {
                                        store.startSession(projectID: project.id, preset: preset)
                                    }
                                }
                            }
                            .padding(.horizontal, 12)
                            .padding(.bottom, listBottomPadding)
                        }
                        .frame(height: listHeight)
                        .scrollBounceBehavior(.basedOnSize)
                    }
                    .frame(maxWidth: min(420, geometry.size.width - 18))
                    // Force dark so the glass buttons (the X) render dark-mode
                    // glass instead of following the system light appearance.
                    .environment(\.colorScheme, .dark)
                    // Real iOS 26 Liquid Glass (`.glassEffect`) — the old
                    // `.ultraThinMaterial` reads as a flat frosted panel;
                    // material fallback below iOS 26.
                    .glassSheetBackground(cornerRadius: 28)
                    .compositingGroup()
                    .shadow(color: .black.opacity(0.42), radius: 28, x: 0, y: -12)
                    .padding(.horizontal, 9)
                    .padding(.bottom, 8)
                    .offset(y: dragOffset)
                    .transition(.move(edge: .bottom).combined(with: .opacity))
                }
            }
            .frame(width: geometry.size.width, height: geometry.size.height)
            .allowsHitTesting(project != nil)
        }
        .ignoresSafeArea()
        .animation(.timingCurve(0.16, 1, 0.3, 1, duration: 0.30), value: store.presetDrawerProjectID)
        // Reset the drag offset whenever the drawer (re)opens so a prior
        // swipe-dismiss can't leave it shifted the next time it appears.
        .onChange(of: store.presetDrawerProjectID) { _, newValue in
            if newValue != nil { dragOffset = 0 }
        }
    }

    /// Swipe the sheet down to dismiss, matching native sheet behavior.
    ///
    /// Measured in `.global` space on purpose: the gesture lives on the header,
    /// which is itself translated by `dragOffset`, so a `.local` translation
    /// would be read in a coordinate space that this gesture is actively
    /// moving — a feedback loop that makes the sheet jump/oscillate. Global
    /// (screen) space is unaffected by the sheet's own offset.
    private var dismissDrag: some Gesture {
        DragGesture(minimumDistance: 8, coordinateSpace: .global)
            .onChanged { value in
                dragOffset = max(0, value.translation.height)
            }
            .onEnded { value in
                let dismiss = value.translation.height > 90
                    || value.predictedEndTranslation.height > 200
                if dismiss {
                    store.hidePresetDrawer()
                } else {
                    withAnimation(.spring(response: 0.32, dampingFraction: 0.86)) {
                        dragOffset = 0
                    }
                }
            }
    }
}

private extension View {
    /// Clean solid dark sheet background. Liquid Glass (`.glassEffect`) was
    /// tried here and looked bad: over the near-black terminal it has nothing
    /// to refract, so `.regular` goes milky-light and a dark tint just reads as
    /// a flat panel that picks up the content behind it inconsistently. A solid
    /// dark fill with a hairline edge is consistent and legible.
    func glassSheetBackground(cornerRadius: CGFloat) -> some View {
        let shape = RoundedRectangle(cornerRadius: cornerRadius, style: .continuous)
        return background(shape.fill(Color(hex: 0x1B1C22)))
            .overlay(
                shape.strokeBorder(Color.white.opacity(0.08), lineWidth: 1)
            )
    }
}

/// In-session Allow / Don't Allow prompt for a pending MCP approval on the
/// connected Host (session write / browser / computer / app-open). Renders
/// the Host-resolved copy verbatim; answering races the desktop prompt and
/// other controllers — first answer wins, the rest dismiss on their own. No
/// tap-outside-to-dismiss: this is a privilege grant, so the only exits are
/// the two explicit answers. Lives on the terminal detail, not a root
/// overlay, so the prompt sits on the Session it belongs to.
struct ApprovalPromptOverlay: View {
    var store: RemotePreviewStore
    var approval: RemotePendingApproval

    var body: some View {
        let moreWaiting = max(
            0,
            store.pendingApprovalCount(forSessionID: approval.presentationSessionID(
                knownIDs: store.mcpApprovalKnownSessionIDs
            )) - 1
        )
        ZStack {
            Color.black.opacity(0.28)
            VStack(spacing: 14) {
                Image(systemName: Self.icon(for: approval.kind))
                    .font(.system(size: 30, weight: .semibold))
                    .foregroundStyle(.yellow)
                Text(approval.title)
                    .font(.system(size: 16, weight: .semibold))
                    .multilineTextAlignment(.center)
                    .fixedSize(horizontal: false, vertical: true)
                Text(approval.body)
                    .font(.system(size: 13))
                    .foregroundStyle(.secondary)
                    .multilineTextAlignment(.center)
                    .fixedSize(horizontal: false, vertical: true)
                if moreWaiting > 0 {
                    Text("\(moreWaiting) more waiting")
                        .font(.system(size: 11))
                        .foregroundStyle(.tertiary)
                }
                HStack(spacing: 10) {
                    Button {
                        Task { await store.answerApproval(id: approval.id, approved: false) }
                    } label: {
                        Text("Don't Allow")
                            .frame(maxWidth: .infinity)
                    }
                    .buttonStyle(.bordered)
                    Button {
                        Task { await store.answerApproval(id: approval.id, approved: true) }
                    } label: {
                        Text("Allow")
                            .frame(maxWidth: .infinity)
                    }
                    .buttonStyle(.borderedProminent)
                }
                .padding(.top, 4)
            }
            .padding(22)
            .frame(maxWidth: 340)
            .glassSheetBackground(cornerRadius: 22)
            .padding(.horizontal, 28)
        }
        .environment(\.colorScheme, .dark)
    }

    private static func icon(for kind: String) -> String {
        switch kind {
        case "write": return "keyboard"
        case "browser": return "globe"
        case "computer": return "desktopcomputer"
        case "app-open": return "square.grid.2x2"
        default: return "questionmark.circle"
        }
    }
}

private struct PresetDrawerRow: View {
    let preset: RemotePresetSummary
    let launching: Bool
    let onLaunch: () -> Void

    /// The CLI type as the title (so the command isn't repeated on both lines).
    /// A custom preset with its own label keeps that label instead.
    private var titleText: String {
        let label = preset.label.trimmingCharacters(in: .whitespacesAndNewlines)
        if !label.isEmpty && label != preset.command {
            return label
        }
        return preset.cliID ?? label
    }

    var body: some View {
        Button(action: onLaunch) {
            HStack(spacing: 11) {
                Group {
                    if launching {
                        Image(systemName: "circle.dotted")
                            .font(.system(size: 16, weight: .semibold))
                            .foregroundStyle(IOSSidebarTheme.toolColor(for: preset))
                    } else {
                        // The real CLI brand logo (same shared SVGs as the
                        // session rows and the desktop app), not an SF Symbol.
                        SharedToolIconView(
                            providerID: preset.cliID,
                            command: preset.command,
                            size: 18
                        )
                    }
                }
                .frame(width: 28, height: 28)
                .background(
                    RoundedRectangle(cornerRadius: 9, style: .continuous)
                        .fill(IOSSidebarTheme.hoverRow)
                )

                VStack(alignment: .leading, spacing: 2) {
                    Text(titleText)
                        .font(.system(size: 14, weight: .semibold))
                        .foregroundStyle(IOSSidebarTheme.foreground)
                        .lineLimit(1)
                    Text(preset.command)
                        .font(.system(size: 11, design: .monospaced))
                        .foregroundStyle(IOSSidebarTheme.mutedForeground)
                        .lineLimit(1)
                        .truncationMode(.middle)
                }

                Spacer(minLength: 0)

                Image(systemName: "arrow.up.right")
                    .font(.system(size: 12, weight: .semibold))
                    .foregroundStyle(IOSSidebarTheme.mutedForeground)
            }
            .padding(EdgeInsets(top: 9, leading: 10, bottom: 9, trailing: 10))
            .frame(maxWidth: .infinity, minHeight: 52, alignment: .leading)
            .background(
                RoundedRectangle(cornerRadius: 14, style: .continuous)
                    .fill(IOSSidebarTheme.hoverRow.opacity(0.72))
            )
        }
        .buttonStyle(.plain)
        .disabled(launching)
        .opacity(launching ? 0.72 : 1)
    }
}

struct SessionSidebarView: View {
    var store: RemotePreviewStore
    @EnvironmentObject private var connection: RemoteConnectionStore
    /// Phone-side push health, so a broken notification pipeline (permission
    /// denied, APNs registration failed) is visible where the user actually
    /// lives — otherwise needs-input alerts just silently stop arriving.
    @ObservedObject private var push = PushManager.shared
    var topContentInset: CGFloat = 0
    /// Disable list scrolling while the drawer is being slid horizontally, so a
    /// slide gesture never scrolls the sidebar at the same time.
    var scrollLocked: Bool = false

    /// Window top inset captured once per view identity — the previous
    /// computed property walked `UIApplication.connectedScenes` twice per
    /// body evaluation of the sidebar, its hottest view.
    @State private var deviceTopInset = Self.deviceWindowTopInset()
    /// Window bottom inset (home indicator), so the pinned feedback footer
    /// clears it — the drawer ignores safe areas.
    @State private var deviceBottomInset = Self.deviceWindowBottomInset()

    /// The drawer overlay ignores safe areas, so the passed inset can arrive
    /// as zero — fall back to the window's real top inset so the header (and
    /// its "+" buttons) never sit under the notch/Dynamic Island.
    private var effectiveTopInset: CGFloat {
        max(topContentInset, deviceTopInset)
    }

    private static func deviceWindowTopInset() -> CGFloat {
        #if os(iOS)
        UIApplication.shared.connectedScenes
            .compactMap { ($0 as? UIWindowScene)?.keyWindow }
            .first?.safeAreaInsets.top ?? 0
        #else
        0
        #endif
    }

    private static func deviceWindowBottomInset() -> CGFloat {
        #if os(iOS)
        UIApplication.shared.connectedScenes
            .compactMap { ($0 as? UIWindowScene)?.keyWindow }
            .first?.safeAreaInsets.bottom ?? 0
        #else
        0
        #endif
    }

    var body: some View {
        ZStack {
            IOSSidebarTheme.background(tintHue: store.hostTintHue).ignoresSafeArea()
            VStack(spacing: 0) {
                // Pinned chrome above the scrolling region, the way the
                // feedback footer is pinned below it: ONE combined
                // workspace + connection header (tap opens the Workspaces
                // sheet), plus the push warning when delivery is broken —
                // both stay put however far the session list scrolls.
                SidebarWorkspaceHeader(
                    store: store,
                    usingRelay: connection.usingRelay
                ) {
                    connection.pairingSheetPresented = true
                }
                .padding(.horizontal, 8)
                .padding(.top, effectiveTopInset + 14)

                // Broken notifications stay visible however far the list is
                // scrolled. Tap opens the settings sheet — its Notifications
                // section has the fix-it buttons (Open iOS Settings / retry).
                if let warning = push.registrationState.sidebarWarning {
                    SidebarPushWarningBanner(message: warning) {
                        connection.pairingSheetPresented = true
                    }
                    .padding(.horizontal, 8)
                    .padding(.top, 6)
                }

                ScrollViewReader { proxy in
                    ScrollView {
                        LazyVStack(alignment: .leading, spacing: 1) {
                            if store.isDisconnected {
                                // No live connection means the session list
                                // is stale by definition — say so instead of
                                // rendering sessions the phone can't reach.
                                SidebarDisconnectedView()
                            } else {
                                listContent
                                    .id("main")
                            }
                        }
                        .padding(EdgeInsets(
                            // Starts just past the mask's top fade band so the
                            // resting list is unfaded; scrolled rows dissolve
                            // in that band as they slide toward the pinned
                            // header instead of clipping hard against it.
                            top: 16,
                            leading: 8,
                            // Breathing room so the last folder/sessions aren't
                            // flush against the bottom edge (and the home
                            // indicator), and the scroll-reveal has room.
                            bottom: 40,
                            trailing: 8
                        ))
                    }
                    .scrollIndicators(.hidden)
                    .scrollDisabled(scrollLocked)
                    .mask(SidebarListFadeMask())
                    .onAppear {
                        // The drawer builds fresh on every open, so this
                        // covers presentation too: position the list without
                        // animation instead of animating a scroll into the
                        // middle of the slide-in.
                        revealSelectedSession(using: proxy, animated: false)
                    }
                    .onChange(of: store.selectedSessionID) { _ in
                        revealSelectedSession(using: proxy, animated: true)
                    }
                    .onChange(of: store.snapshot.paneGroups) { _ in
                        // A bootstrap can add/remove a pane group without
                        // changing selection. Keep a selected child visible
                        // when its presentation home moves under a different
                        // representative/project.
                        revealSelectedSession(using: proxy, animated: false)
                    }
                }
                feedbackFooter
            }
        }
        .environment(\.colorScheme, .dark)
        .onAppear {
            // Refresh in case the key window wasn't attached yet when the
            // @State initial value was captured.
            let inset = Self.deviceWindowTopInset()
            if deviceTopInset != inset { deviceTopInset = inset }
            let bottom = Self.deviceWindowBottomInset()
            if deviceBottomInset != bottom { deviceBottomInset = bottom }
        }
    }

    /// GitHub Discussions — same destination as the website footer's
    /// "Bugs & Feedback" link.
    private static let feedbackURL = URL(string: "https://github.com/orgs/unpeel-com/discussions")!

    /// Pinned at the very bottom of the sidebar: opens the GitHub discussion.
    private var feedbackFooter: some View {
        Link(destination: Self.feedbackURL) {
            HStack(spacing: 8) {
                Image(systemName: "exclamationmark.bubble")
                    .font(.system(size: 13, weight: .medium))
                Text("Feedback & bugs")
                    .font(.system(size: 13, weight: .medium))
                Spacer(minLength: 4)
                Image(systemName: "arrow.up.right")
                    .font(.system(size: 11, weight: .semibold))
                    .opacity(0.7)
            }
            .foregroundStyle(IOSSidebarTheme.mutedForeground)
            .padding(.horizontal, 14)
            .padding(.top, 12)
            .padding(.bottom, max(deviceBottomInset, 12))
            .frame(maxWidth: .infinity, alignment: .leading)
            .contentShape(Rectangle())
        }
        .buttonStyle(.plain)
        .overlay(alignment: .top) {
            Rectangle()
                .fill(.white.opacity(0.08))
                .frame(height: 1)
        }
    }

    @ViewBuilder
    private var listContent: some View {
        ForEach(folderGroups, id: \.folder.id) { group in
            let isOpen = store.expandedFolderID == group.folder.id
            FolderSectionRow(
                folder: group.folder,
                isExpanded: isOpen,
                sessionCount: group.projects.reduce(0) {
                    $0 + sessionCount(for: $1, includeChildren: true)
                },
                activity: isOpen ? (false, false) : folderActivity(group.projects)
            ) {
                withAnimation(.timingCurve(0.16, 1, 0.3, 1, duration: 0.28)) {
                    store.toggleFolder(group.folder.id)
                }
            }
            if isOpen {
                ForEach(group.projects) { project in
                    projectBlock(project)
                }
            }
        }

        ForEach(looseProjects) { project in
            projectBlock(project)
        }
    }

    @ViewBuilder
    private func projectBlock(
        _ project: RemoteProjectSummary,
        depth: Int = 0
    ) -> some View {
        let projectSessions = sessions(for: project)
        let children = inlineChildProjects(for: project)
        let descendantSessions = children.flatMap { sessions(for: $0) }
        let isExpanded = store.expandedProjectIDs.contains(project.id)
        Group {
            MacStyleProjectRow(
                project: project,
                depth: depth,
                isExpanded: isExpanded,
                hasBusySession: (projectSessions + descendantSessions).contains {
                    $0.activity == .working
                },
                hasAttentionSession: (projectSessions + descendantSessions).contains {
                    $0.activity == .blocked
                        || store.sessionNeedsMcpApprovalAttention($0.id)
                },
                canCreateSession: store.supportsSessionCreation,
                onAdd: {
                    store.showPresetDrawer(for: project.id)
                },
                onOrganize: { store.presentProjectOrganize(for: project) }
            ) {
                withAnimation(.timingCurve(0.16, 1, 0.3, 1, duration: 0.28)) {
                    store.toggleProject(project.id)
                }
            }
            if isExpanded {
                // Pinned partition, at the top like the desktop: pinned
                // sessions (incl. hoisted project-sidebar members) first, then
                // pinned child groups — never left down in the regular section.
                ForEach(pinnedSessions(for: project)) { session in
                    sessionCluster(session, project: project, depth: depth + 1)
                }
                ForEach(pinnedChildFolders(for: project)) { child in
                    childFolderBlock(child, depth: depth + 1)
                }
                ForEach(mixedRegularItems(for: project)) { item in
                    switch item {
                    case .folder(let child):
                        childFolderBlock(child, depth: depth + 1)
                    case .session(let session):
                        sessionCluster(session, project: project, depth: depth + 1)
                    }
                }
            }
        }
    }

    @ViewBuilder
    private func childFolderBlock(
        _ project: RemoteProjectSummary,
        depth: Int
    ) -> some View {
        let projectSessions = sessions(for: project)
        let isExpanded = store.expandedProjectIDs.contains(project.id)
        MacStyleProjectRow(
            project: project,
            depth: depth,
            isExpanded: isExpanded,
            hasBusySession: projectSessions.contains { $0.activity == .working },
            hasAttentionSession: projectSessions.contains {
                $0.activity == .blocked || store.sessionNeedsMcpApprovalAttention($0.id)
            },
            canCreateSession: store.supportsSessionCreation,
            onAdd: { store.showPresetDrawer(for: project.id) },
            onOrganize: { store.presentProjectOrganize(for: project) }
        ) {
            withAnimation(.timingCurve(0.16, 1, 0.3, 1, duration: 0.28)) {
                store.toggleProject(project.id)
            }
        }
        if isExpanded {
            projectSessionRows(for: project, depth: depth + 1)
        }
    }

    @ViewBuilder
    private func projectSessionRows(
        for project: RemoteProjectSummary,
        depth: Int
    ) -> some View {
        ForEach(pinnedSessions(for: project)) { session in
            sessionCluster(session, project: project, depth: depth)
        }
        ForEach(displayedRegularSessions(for: project)) { session in
            sessionCluster(session, project: project, depth: depth)
        }
    }

    /// Match the desktop sidebar's pane presentation without trying to put a
    /// multi-terminal layout on the phone: the representative keeps its
    /// ordinary row and every other pane Session is attached immediately
    /// beneath it. Selecting a child still opens that Session's one full-screen
    /// terminal.
    @ViewBuilder
    private func sessionCluster(
        _ representative: RemoteSessionSummary,
        project: RemoteProjectSummary,
        depth: Int
    ) -> some View {
        sessionRow(
            representative,
            project: project,
            depth: depth,
            paneChild: false
        )
        ForEach(tree.paneChildren(for: representative)) { child in
            sessionRow(
                child,
                project: store.projectsByID[child.projectID] ?? project,
                depth: depth,
                paneChild: true
            )
        }
    }

    private func sessionRow(
        _ session: RemoteSessionSummary,
        project: RemoteProjectSummary,
        depth: Int,
        paneChild: Bool
    ) -> some View {
        MacStyleSessionRow(
            session: session,
            project: project,
            selected: session.id == store.selectedSessionID,
            depth: depth,
            paneChild: paneChild,
            needsAttention: store.sessionNeedsMcpApprovalAttention(session.id),
            tintHue: store.hostTintHue,
            onOrganize: { store.presentSessionOrganize(for: session) }
        ) {
            store.select(session)
        }
        .id(session.id)
        .transition(.opacity.combined(with: .move(edge: .top)))
    }

    private func mixedRegularItems(
        for project: RemoteProjectSummary
    ) -> [IOSSidebarMixedItem] {
        IOSSidebarProjectTree.mixedRegularItems(
            sessions: displayedRegularSessions(for: project),
            folders: regularChildFolders(for: project),
            order: project.sessionOrder,
            dateSorted: project.dateSorted == true
        )
    }

    private static let visibleSessionLimit = 5

    private var tree: IOSSidebarProjectTree {
        store.sidebarTree
    }

    private var looseProjects: [RemoteProjectSummary] {
        tree.looseProjects
    }

    private var folderGroups: [(folder: RemoteProjectFolderSummary, projects: [RemoteProjectSummary])] {
        tree.folderGroups
    }

    private func inlineChildProjects(for project: RemoteProjectSummary) -> [RemoteProjectSummary] {
        tree.childProjects(for: project).filter {
            $0.isGroup == true || store.worktreesEnabled
        }
    }

    /// Pinned child groups belong to the pinned partition at the top (desktop
    /// parity); the rest interleave in the regular section.
    private func pinnedChildFolders(for project: RemoteProjectSummary) -> [RemoteProjectSummary] {
        inlineChildProjects(for: project).filter { $0.pinned == true }
    }

    private func regularChildFolders(for project: RemoteProjectSummary) -> [RemoteProjectSummary] {
        inlineChildProjects(for: project).filter { $0.pinned != true }
    }

    private func sessionCount(
        for project: RemoteProjectSummary,
        includeChildren: Bool,
        visited: Set<String> = []
    ) -> Int {
        guard !visited.contains(project.id) else { return 0 }
        let direct = sessions(for: project).count
        guard includeChildren else { return direct }
        let nextVisited = visited.union([project.id])
        return direct + inlineChildProjects(for: project).reduce(0) { total, child in
            total + sessionCount(for: child, includeChildren: true, visited: nextVisited)
        }
    }

    private func folderActivity(
        _ projects: [RemoteProjectSummary]
    ) -> (busy: Bool, attention: Bool) {
        let visibleProjects = projects.flatMap { [$0] + inlineChildProjects(for: $0) }
        let visibleSessions = visibleProjects.flatMap { sessions(for: $0) }
        return (
            visibleSessions.contains { $0.activity == .working },
            visibleSessions.contains {
                $0.activity == .blocked || store.sessionNeedsMcpApprovalAttention($0.id)
            }
        )
    }

    private func sessions(for project: RemoteProjectSummary) -> [RemoteSessionSummary] {
        tree.sessions(for: project)
    }

    private func pinnedSessions(for project: RemoteProjectSummary) -> [RemoteSessionSummary] {
        sessions(for: project).filter {
            ($0.pinned || tree.isProjectSidebarMember($0))
                && !$0.archived && !tree.isPaneChild($0)
        }
    }

    private func regularSessions(for project: RemoteProjectSummary) -> [RemoteSessionSummary] {
        sessions(for: project).filter {
            // Live project-sidebar members show pinned at top; only their
            // archived rows fall back into the ordinary list.
            (!tree.isProjectSidebarMember($0) || $0.archived)
                && (!$0.pinned || $0.archived)
                && !tree.isPaneChild($0)
        }
    }

    /// The desktop sidebar model, mirrored: running sessions always render,
    /// followed by at most 5 naturally stopped and archived sessions combined;
    /// selected/unread/working/blocked inactive rows always stay. A new Host
    /// already sends the list partitioned and windowed, so this is a stable
    /// no-op there; against an older Mac (full interleaved list) it applies the
    /// same model phone-side.
    private func displayedRegularSessions(for project: RemoteProjectSummary) -> [RemoteSessionSummary] {
        let regular = regularSessions(for: project)
        let active = regular.filter { !$0.archived && $0.status == .running }
        let stopped = regular.filter { !$0.archived && $0.status != .running }
        let archived = regular.filter(\.archived)
        var inactiveRank = 0
        return active + (stopped + archived).compactMap { session in
            let insidePreview = inactiveRank < Self.visibleSessionLimit
            inactiveRank += 1
            if insidePreview || session.id == store.selectedSessionID || session.unread
                || session.activity == .working || session.activity == .blocked
                || store.sessionNeedsMcpApprovalAttention(session.id) {
                return session
            }
            return nil
        }
    }

    private func revealSelectedSession(using proxy: ScrollViewProxy, animated: Bool) {
        guard let selected = store.selectedSession else { return }
        // A pane child is rendered under its representative, which may belong
        // to another project/folder. Reveal the row's presentation home rather
        // than the child's otherwise-hidden source location.
        let presentationSession = tree.paneRepresentative(for: selected) ?? selected
        store.revealProject(presentationSession.projectID)
        if animated {
            DispatchQueue.main.asyncAfter(deadline: .now() + 0.12) {
                withAnimation(.easeOut(duration: 0.22)) {
                    proxy.scrollTo(selected.id, anchor: .center)
                }
            }
        } else {
            // Next runloop tick: the expansion above has to land in the tree
            // before the target row exists to scroll to.
            DispatchQueue.main.async {
                proxy.scrollTo(selected.id, anchor: .center)
            }
        }
    }

}

/// Sidebar-shaped index of a bootstrap snapshot. Everything is precomputed in
/// the initializer — the sidebar reads these per row per render pass, and the
/// previous lazy filters were O(projects × sessions) each time.
struct IOSSidebarProjectTree {
    let looseProjects: [RemoteProjectSummary]
    let folderGroups: [(folder: RemoteProjectFolderSummary, projects: [RemoteProjectSummary])]
    private let childrenByParent: [String: [RemoteProjectSummary]]
    private let sessionsByProject: [String: [RemoteSessionSummary]]
    private let projectsByID: [String: RemoteProjectSummary]
    private let paneChildrenByRepresentativeID: [String: [RemoteSessionSummary]]
    private let paneRepresentativeByChildID: [String: RemoteSessionSummary]
    /// Sessions that were filed in a project-sidebar storage group and hoisted
    /// into the parent project's list; the phone shows them pinned at the top.
    private let sidebarGroupSessionIDs: Set<String>

    init(snapshot: RemoteBootstrapSnapshot) {
        let folderIDs = Set(snapshot.folders.map(\.id))

        func isMainTreeProject(
            _ project: RemoteProjectSummary,
            acceptingLegacyFolderParentID legacyFolderID: String? = nil
        ) -> Bool {
            guard !project.isInlineSidebarFolder else { return false }
            guard let parentID = project.parentProjectID else { return true }
            if parentID == legacyFolderID { return true }
            return folderIDs.contains(parentID)
        }

        looseProjects = snapshot.projects.filter { project in
            project.folderID == nil && isMainTreeProject(project)
        }
        folderGroups = snapshot.folders.map { folder in
            let projects = snapshot.projects.filter { project in
                project.folderID == folder.id
                    && isMainTreeProject(project, acceptingLegacyFolderParentID: folder.id)
            }
            return (folder, projects)
        }
        .filter { !$0.projects.isEmpty }
        // The desktop's project-sidebar storage group is presentation-hidden
        // on the phone: the group row never renders, and its member sessions
        // hoist into the parent project — shown pinned at the top of that
        // project (the phone's stand-in for the desktop right-side stack).
        let sidebarGroupParents: [String: String] = Dictionary(
            uniqueKeysWithValues: snapshot.projects.compactMap { project in
                guard project.isProjectSidebarGroup,
                      let parent = project.parentProjectID
                else { return nil }
                return (project.id, parent)
            }
        )
        childrenByParent = Dictionary(
            grouping: snapshot.projects.filter {
                $0.isInlineSidebarFolder && !$0.isProjectSidebarGroup
            },
            by: { $0.parentProjectID ?? "" }
        )
        let supportedSessions = snapshot.sessions.filter(\.supportsIOSSessionAPI)
        sidebarGroupSessionIDs = Set(
            supportedSessions.compactMap { session in
                sidebarGroupParents[session.projectID] != nil ? session.id : nil
            }
        )
        sessionsByProject = Dictionary(
            grouping: supportedSessions,
            by: { sidebarGroupParents[$0.projectID] ?? $0.projectID }
        )
        .mapValues { Self.treeOrdered($0) }
        projectsByID = Dictionary(uniqueKeysWithValues: snapshot.projects.map { ($0.id, $0) })

        // Pane summaries cross a trust/version boundary. Accept only complete,
        // non-overlapping groups whose representative and at least one child
        // are present in this bootstrap. A stale or malformed group therefore
        // leaves its Sessions flat instead of accidentally hiding a row.
        let sessionsByID = Dictionary(
            supportedSessions.map { ($0.id, $0) },
            uniquingKeysWith: { first, _ in first }
        )
        var claimedSessionIDs = Set<String>()
        var childrenByRepresentative: [String: [RemoteSessionSummary]] = [:]
        var representativeByChild: [String: RemoteSessionSummary] = [:]
        for group in snapshot.paneGroups ?? [] {
            guard let representative = sessionsByID[group.representativeSessionID],
                  !representative.archived
            else { continue }

            var seenInGroup = Set<String>()
            let orderedMembers = group.sessionIDs.compactMap { id -> RemoteSessionSummary? in
                guard seenInGroup.insert(id).inserted,
                      let session = sessionsByID[id],
                      !session.archived
                else { return nil }
                return session
            }
            guard orderedMembers.count >= 2,
                  orderedMembers.contains(where: { $0.id == representative.id })
            else { continue }

            let memberIDs = Set(orderedMembers.map(\.id))
            guard claimedSessionIDs.isDisjoint(with: memberIDs) else { continue }
            let children = orderedMembers.filter { $0.id != representative.id }
            guard !children.isEmpty else { continue }

            claimedSessionIDs.formUnion(memberIDs)
            childrenByRepresentative[representative.id] = children
            for child in children {
                representativeByChild[child.id] = representative
            }
        }
        paneChildrenByRepresentativeID = childrenByRepresentative
        paneRepresentativeByChildID = representativeByChild
    }

    /// Current Macs already ship sessions in exact desktop order. Keep that
    /// order unchanged; session hierarchy no longer exists.
    private static func treeOrdered(
        _ sessions: [RemoteSessionSummary]
    ) -> [RemoteSessionSummary] {
        sessions
    }

    func worktreeProjects(for project: RemoteProjectSummary) -> [RemoteProjectSummary] {
        childProjects(for: project).filter { $0.worktreeBranch != nil }
    }

    func childProjects(for project: RemoteProjectSummary) -> [RemoteProjectSummary] {
        childrenByParent[project.id] ?? []
    }

    func sessions(for project: RemoteProjectSummary) -> [RemoteSessionSummary] {
        sessionsByProject[project.id] ?? []
    }

    func paneChildren(for representative: RemoteSessionSummary) -> [RemoteSessionSummary] {
        paneChildrenByRepresentativeID[representative.id] ?? []
    }

    func isPaneChild(_ session: RemoteSessionSummary) -> Bool {
        paneRepresentativeByChildID[session.id] != nil
    }

    /// A session hoisted out of a hidden project-sidebar storage group. The
    /// phone pins these to the top of the parent project.
    func isProjectSidebarMember(_ session: RemoteSessionSummary) -> Bool {
        sidebarGroupSessionIDs.contains(session.id)
    }

    func paneRepresentative(for session: RemoteSessionSummary) -> RemoteSessionSummary? {
        paneRepresentativeByChildID[session.id]
    }

    /// Regular-section rows: sessions and child folders interleaved when
    /// the Host's session-order list contains a folder id. Until then,
    /// folders stay above sessions (the previous layout).
    static func mixedRegularItems(
        sessions: [RemoteSessionSummary],
        folders: [RemoteProjectSummary],
        order: [String]?,
        dateSorted: Bool
    ) -> [IOSSidebarMixedItem] {
        let pinnedGroupIDs = Set(folders.compactMap { folder in
            folder.pinned == true ? folder.id : nil
        })
        func pinnedGroupsFirst(_ items: [IOSSidebarMixedItem]) -> [IOSSidebarMixedItem] {
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
        func archivedLast(_ items: [IOSSidebarMixedItem]) -> [IOSSidebarMixedItem] {
            let archives = items.filter { item in
                if case .session(let session) = item { return session.archived }
                return false
            }
            return items.filter { item in
                if case .session(let session) = item { return !session.archived }
                return true
            } + archives
        }
        guard !folders.isEmpty else {
            return sessions.map { .session($0) }
        }
        let folderIDs = Set(folders.map(\.id))
        let rankedIDs = order ?? []
        let mixed = rankedIDs.contains { folderIDs.contains($0) }
        if dateSorted || !mixed {
            return archivedLast(
                pinnedGroupsFirst(
                    folders.map { .folder($0) } + sessions.map { .session($0) }
                )
            )
        }
        var byID: [String: IOSSidebarMixedItem] = [:]
        for folder in folders { byID[folder.id] = .folder(folder) }
        for session in sessions { byID[session.id] = .session(session) }
        var seen = Set<String>()
        var ranked: [IOSSidebarMixedItem] = []
        for id in rankedIDs {
            if let item = byID[id], seen.insert(id).inserted {
                ranked.append(item)
            }
        }
        let unrankedSessions = sessions
            .filter { !seen.contains($0.id) }
            .map { IOSSidebarMixedItem.session($0) }
        let unrankedFolders = folders
            .filter { !seen.contains($0.id) }
            .map { IOSSidebarMixedItem.folder($0) }
        return archivedLast(
            pinnedGroupsFirst(unrankedSessions + unrankedFolders + ranked)
        )
    }

    func revealIDs(forProjectID projectID: String) -> Set<String> {
        var ids: Set<String> = [projectID]
        if let project = projectsByID[projectID],
           project.isInlineSidebarFolder,
           let parentID = project.parentProjectID {
            ids.insert(parentID)
        }
        return ids
    }
}

private extension RemoteProjectSummary {
    var isInlineSidebarFolder: Bool {
        parentProjectID != nil && (isGroup == true || worktreeBranch != nil)
    }

    /// The Mac's per-project "Sidebar" group (deterministic id) — pure
    /// storage for the desktop's right panel. The phone hides the group row
    /// and flattens its members into the parent project's list; a dedicated
    /// mobile presentation for panel panes is TBD.
    var isProjectSidebarGroup: Bool {
        guard isGroup == true, let parent = parentProjectID else { return false }
        return id == "sidebar-" + parent
    }
}

/// One regular-section row: a session or a child group/worktree.
enum IOSSidebarMixedItem: Identifiable, Equatable {
    case session(RemoteSessionSummary)
    case folder(RemoteProjectSummary)

    var id: String {
        switch self {
        case .session(let session): return session.id
        case .folder(let folder): return folder.id
        }
    }
}

enum IOSSidebarTheme {
    static let foreground = Color(hex: 0xF3F5FB)
    static let mutedForeground = Color(hex: 0xF3F5FB, opacity: 0.66)
    static let hoverRow = Color(hex: 0xF3F5FB, opacity: 0.10)
    static let activeRow = Color(hex: 0xFFFFFF, opacity: 0.16)

    /// Selected-row chip washed toward the active Host's workspace tint —
    /// the desktop's dark-mode `Theme.activeRowGlassTint` formula, so the
    /// active session reads the same color on both platforms and differs
    /// per workspace. nil hue = the neutral `activeRow` above.
    static func activeRow(tintHue: Double?) -> Color {
        guard let hue = tintHue else { return activeRow }
        return Color(hue: hue / 360, saturation: 0.55, brightness: 0.52, opacity: 0.30)
    }

    static let attention = Color(hex: 0xF59E0B)
    static let unread = Color(hex: 0x60A5FA)
    static let background = LinearGradient(
        colors: [
            Color(hex: TerminalChrome.frameBackgroundHex),
            Color(hex: TerminalChrome.backgroundHex),
        ],
        startPoint: .bottom,
        endPoint: .top
    )

    /// The sidebar gradient washed toward the active Host's tint (nil = the
    /// shipped neutral gradient above, byte-identical).
    static func background(tintHue: Double?) -> LinearGradient {
        LinearGradient(
            colors: [
                TerminalChrome.hostTinted(
                    hex: TerminalChrome.frameBackgroundHex, tintHue: tintHue
                ),
                TerminalChrome.hostTinted(
                    hex: TerminalChrome.backgroundHex, tintHue: tintHue
                ),
            ],
            startPoint: .bottom,
            endPoint: .top
        )
    }

    static func toolColor(for command: String) -> Color {
        if command.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty {
            return Color(hex: 0xD6D9E1)
        }
        if let hex = UnpeelRuntimeCatalog.runtime(command: command)?.tintColorHex {
            return Color(hex: UInt32(hex))
        }
        return Color(hex: 0xB9BDC9)
    }

    static func toolSpinnerColor(for command: String) -> Color {
        if let runtime = UnpeelRuntimeCatalog.runtime(command: command),
           let hex = runtime.spinnerTintColorHex ?? runtime.tintColorHex {
            return Color(hex: UInt32(hex))
        }
        return toolColor(for: command)
    }

    /// Mac-resolved spinner tint when the summary carries one (the single
    /// source of truth — new CLIs get their color with no phone update);
    /// the legacy command-prefix table above is the fallback for older Macs.
    static func toolSpinnerColor(for session: RemoteSessionSummary) -> Color {
        if let hex = session.spinnerColorHex { return Color(hex: UInt32(hex)) }
        return toolSpinnerColor(for: session.command)
    }

    /// Same Mac-first resolution for preset tints (drawer rows).
    static func toolColor(for preset: RemotePresetSummary) -> Color {
        if let hex = preset.tintColorHex { return Color(hex: UInt32(hex)) }
        return toolColor(for: preset.command)
    }

    /// Same dark-mode folder palette as the Mac app and TUI. A missing color
    /// stays neutral; only explicitly colored folders pick up a tint.
    static func folderColor(for id: String?) -> Color? {
        switch id {
        case "sky": return Color(hex: 0x7DD3FC)
        case "blue": return Color(hex: 0x7EA6FF)
        case "violet": return Color(hex: 0xB79CFF)
        case "rose": return Color(hex: 0xF79AC0)
        case "amber": return Color(hex: 0xF8C86A)
        case "moss": return Color(hex: 0x9DD67A)
        case "teal": return Color(hex: 0x64DCCB)
        case "graphite": return Color(hex: 0xB8BCC8)
        default: return nil
        }
    }
}

private struct SidebarListFadeMask: View {
    var body: some View {
        VStack(spacing: 0) {
            // Short top band: the list now rests below it (the header is
            // pinned chrome above the scroll region), so it only softens rows
            // actively scrolling out at the top.
            LinearGradient(
                stops: [
                    .init(color: .clear, location: 0),
                    .init(color: .black.opacity(0.5), location: 0.5),
                    .init(color: .black, location: 1),
                ],
                startPoint: .top,
                endPoint: .bottom
            )
            .frame(height: 16)
            Color.black
            LinearGradient(
                stops: [
                    .init(color: .black, location: 0),
                    .init(color: .black.opacity(0.65), location: 0.48),
                    .init(color: .clear, location: 1),
                ],
                startPoint: .top,
                endPoint: .bottom
            )
            .frame(height: 34)
        }
    }
}

/// The ONE pinned sidebar header — the previous scrolling "Connected via …"
/// status row and the workspace picker row beneath it, merged. Shows the
/// current workspace's tint dot + name, the connection state, and the same
/// session-count badge the status row showed. Fixed above the scrolling
/// session list as drawer chrome (the feedback footer's mirror). Tapping
/// opens the **Workspaces** sheet, where each paired record IS a workspace
/// (2026-08-23) — switching rows switches the active connection. This row is
/// a shortcut to that shared switcher, never its own menu.
private struct SidebarWorkspaceHeader: View {
    var store: RemotePreviewStore
    /// True while the connection tunnels through Unpeel Remote — surfaced as
    /// the "via Unpeel Remote" subtitle.
    var usingRelay: Bool = false
    /// Opens the Workspaces sheet (`connection.pairingSheetPresented`).
    var onTap: () -> Void

    private var current: RemoteWorkspaceSummary? { store.currentWorkspace }

    /// The workspace name leads; an older Mac that advertises no workspaces
    /// falls back to the connected Mac's name so the header never goes blank.
    private var title: String {
        current?.name ?? store.snapshot.macName ?? "Workspace"
    }

    /// Connection state as a subtitle only when it is noteworthy: the calm
    /// "Connecting…" grace phase and the failing "Connection lost" while
    /// disconnected, "via Unpeel Remote" on the relay path. A healthy Direct
    /// connection stays quiet (the session list itself is the proof of life).
    private var subtitle: String? {
        if store.isDisconnected { return store.connectionStatus }
        if usingRelay { return "via Unpeel Remote" }
        return nil
    }

    var body: some View {
        Button(action: onTap) {
            HStack(spacing: 7) {
                // The red wifi-slash is failure styling: a young disconnect is
                // still the calm "Connecting…" phase (the store's unreachable
                // grace period), so the workspace keeps its ordinary tint dot
                // until reconnect attempts have persisted long enough to count
                // as failing.
                if store.isUnreachable {
                    Image(systemName: "wifi.slash")
                        .font(.system(size: 12, weight: .medium))
                        .foregroundStyle(IOSSidebarTheme.attention)
                        .frame(width: 16, height: 16)
                } else {
                    WorkspaceTintDot(tintHue: current?.tintHue)
                        .frame(width: 16, height: 16)
                }
                VStack(alignment: .leading, spacing: 1) {
                    HStack(spacing: 5) {
                        Text(title)
                            .font(.system(size: 13, weight: .medium))
                            .foregroundStyle(IOSSidebarTheme.foreground)
                            .lineLimit(1)
                        if let current, let icon = WorkspaceKindBadge.icon(for: current) {
                            // Distinct glyph for a remote Host so the user can
                            // tell a remote from a local workspace at a glance,
                            // even before opening the switcher.
                            Image(systemName: icon)
                                .font(.system(size: 9, weight: .semibold))
                                .foregroundStyle(IOSSidebarTheme.mutedForeground)
                                .accessibilityLabel("Remote host")
                        }
                    }
                    if let subtitle {
                        Text(subtitle)
                            .font(.system(size: 10))
                            .foregroundStyle(IOSSidebarTheme.mutedForeground)
                            .lineLimit(1)
                    }
                }
                Spacer(minLength: 0)
                // The session count is meaningless without a connection.
                if !store.isDisconnected {
                    Text("\(store.snapshot.sessions.filter(\.supportsIOSSessionAPI).count)")
                        .font(.system(size: 10, weight: .medium))
                        .foregroundStyle(IOSSidebarTheme.mutedForeground)
                        .padding(.horizontal, 6)
                        .frame(height: 18)
                        .background(IOSSidebarTheme.hoverRow, in: Capsule())
                }
                Image(systemName: "chevron.up.chevron.down")
                    .font(.system(size: 9, weight: .semibold))
                    .foregroundStyle(IOSSidebarTheme.mutedForeground)
            }
            .padding(EdgeInsets(top: 6, leading: 9, bottom: 6, trailing: 9))
            .background(
                RoundedRectangle(cornerRadius: 9, style: .continuous)
                    .fill(IOSSidebarTheme.hoverRow)
            )
            .contentShape(Rectangle())
        }
        .buttonStyle(.plain)
        .accessibilityLabel(
            "Workspace: \(title). \(subtitle ?? store.connectionStatus). Opens the device and workspace switcher."
        )
    }
}

/// Shared presentation for a workspace's *kind* in the sidebar header. Only
/// the icon remains in use: the Workspaces sheet now lists paired records
/// (one pairing = one workspace, 2026-08-23), so the proxied-entry tag went
/// with the retired cross-workspace switcher.
enum WorkspaceKindBadge {
    /// A distinct SF Symbol for a remote entry on a compact row, or nil for
    /// a local workspace (nil `kind` = local, the back-compat default).
    static func icon(for workspace: RemoteWorkspaceSummary) -> String? {
        switch workspace.kind {
        case "ssh": return "terminal"
        case "paired": return "server.rack"
        default: return nil
        }
    }
}

/// A workspace's App-color dot — its tint hue is the workspace's visual
/// identity. A missing hue reads as a neutral outline dot. The compact
/// sidebar row uses the default size; the Workspaces sheet passes a larger
/// one so the dot reads as the row's identity mark, not a speck.
struct WorkspaceTintDot: View {
    let tintHue: Double?
    var size: CGFloat = 9

    var body: some View {
        Circle()
            .fill(fill)
            .frame(width: size, height: size)
            .overlay(
                Circle().strokeBorder(.white.opacity(0.18), lineWidth: 0.5)
            )
    }

    private var fill: Color {
        guard let tintHue else { return IOSSidebarTheme.mutedForeground.opacity(0.4) }
        return Color(hue: tintHue / 360, saturation: 0.62, brightness: 0.85)
    }
}

/// Sticky under the sidebar's top veil whenever phone-side push delivery is
/// broken (permission denied or APNs registration failed). Opaque-ish so
/// rows scrolling underneath don't fight the label.
private struct SidebarPushWarningBanner: View {
    let message: String
    let onTap: () -> Void

    var body: some View {
        Button(action: onTap) {
            HStack(spacing: 7) {
                Image(systemName: "bell.slash.fill")
                    .font(.system(size: 11, weight: .medium))
                    .foregroundStyle(IOSSidebarTheme.attention)
                    .frame(width: 16, height: 16)
                Text(message)
                    .font(.system(size: 12, weight: .medium))
                    .foregroundStyle(IOSSidebarTheme.foreground)
                    .lineLimit(1)
                Spacer(minLength: 4)
                Image(systemName: "chevron.right")
                    .font(.system(size: 9, weight: .semibold))
                    .foregroundStyle(IOSSidebarTheme.mutedForeground)
            }
            .padding(EdgeInsets(top: 5, leading: 9, bottom: 5, trailing: 9))
            .background(
                RoundedRectangle(cornerRadius: 9, style: .continuous)
                    .fill(.ultraThinMaterial)
                    .overlay(
                        RoundedRectangle(cornerRadius: 9, style: .continuous)
                            .fill(IOSSidebarTheme.attention.opacity(0.16))
                    )
            )
            .contentShape(Rectangle())
        }
        .buttonStyle(.plain)
        .accessibilityLabel("\(message). Opens notification settings.")
    }
}

/// Replaces the session list while the Mac is unreachable: an honest "lost
/// connection" state instead of stale rows that can't be tapped into.
/// Reconnection retries the saved Direct endpoint and the E2E Relay, so this
/// only informs — the pairing sheet stays one tap away via the status row.
private struct SidebarDisconnectedView: View {
    var body: some View {
        VStack(spacing: 10) {
            // Friendlier company than a wifi-slash glyph — the user stares at
            // this while reconnection retries run (see PixelMascotView).
            PixelMascotView(size: 48)
                .padding(.bottom, 2)
            Text("Connection lost")
                .font(.system(size: 14, weight: .semibold))
                .foregroundStyle(IOSSidebarTheme.foreground)
            Text("Reconnecting automatically. Your sessions will reappear once your Mac is reachable.")
                .font(.system(size: 12))
                .foregroundStyle(IOSSidebarTheme.mutedForeground)
                .multilineTextAlignment(.center)
            ProgressView()
                .controlSize(.small)
                .tint(IOSSidebarTheme.mutedForeground)
                .padding(.top, 2)
        }
        .frame(maxWidth: .infinity)
        .padding(.horizontal, 22)
        .padding(.vertical, 36)
    }
}

/// Folder accordion header: tap to open this folder (closing the previous
/// one). Collapsed headers keep the fleet glanceable with aggregate
/// busy/attention dots and a session count.
private struct FolderSectionRow: View {
    let folder: RemoteProjectFolderSummary
    let isExpanded: Bool
    let sessionCount: Int
    let activity: (busy: Bool, attention: Bool)
    let onToggle: () -> Void

    private var folderTint: Color {
        IOSSidebarTheme.folderColor(for: folder.colorID) ?? IOSSidebarTheme.mutedForeground
    }

    var body: some View {
        HStack(spacing: 6) {
            Image(systemName: "chevron.right")
                .font(.system(size: 9, weight: .semibold))
                .foregroundStyle(folderTint)
                .rotationEffect(.degrees(isExpanded ? 90 : 0))
                .frame(width: 12)

            Text(folder.name)
                .font(.system(size: 11, weight: .semibold))
                .foregroundStyle(IOSSidebarTheme.foreground)
                .textCase(.uppercase)
                .lineLimit(1)

            // Attention only — no green "busy" dot; the desktop shows no
            // aggregate busy marker on folder rows either.
            if activity.attention {
                Circle()
                    .fill(IOSSidebarTheme.attention)
                    .frame(width: 6, height: 6)
            }

            Spacer(minLength: 4)

            if !isExpanded && sessionCount > 0 {
                Text("\(sessionCount)")
                    .font(.system(size: 11))
                    .foregroundStyle(IOSSidebarTheme.mutedForeground)
                    .padding(.horizontal, 6)
                    .frame(height: 18)
                    .background(Capsule().fill(IOSSidebarTheme.hoverRow))
            }
        }
        .padding(EdgeInsets(top: 8, leading: 6, bottom: 5, trailing: 9))
        .frame(minHeight: 30)
        .contentShape(Rectangle())
        .onTapGesture(perform: onToggle)
        .accessibilityAddTraits(.isButton)
        .accessibilityLabel("\(folder.name), \(isExpanded ? "expanded" : "collapsed")")
    }
}

private struct MacStyleProjectRow: View {
    let project: RemoteProjectSummary
    let depth: Int
    let isExpanded: Bool
    let hasBusySession: Bool
    let hasAttentionSession: Bool
    let canCreateSession: Bool
    let onAdd: () -> Void
    /// Long-press: the folder organize sheet (rename/sort/color/archive) —
    /// the phone's stand-in for the desktop project context menu.
    var onOrganize: (() -> Void)? = nil
    let onToggle: () -> Void

    private var isChildFolder: Bool { project.isInlineSidebarFolder }
    private var folderTint: Color {
        IOSSidebarTheme.folderColor(for: project.colorID) ?? IOSSidebarTheme.mutedForeground
    }

    var body: some View {
        HStack(spacing: 7) {
            Group {
                if isChildFolder {
                    Image(systemName: "chevron.right")
                        .font(.system(size: 11, weight: .semibold))
                        .foregroundStyle(folderTint)
                        .rotationEffect(.degrees(isExpanded ? 90 : 0))
                        .animation(.easeInOut(duration: 0.15), value: isExpanded)
                } else {
                    SharedChromeIconView(
                        icon: isExpanded ? .folderOpen : .folderClosed,
                        size: 18
                    )
                    .foregroundStyle(folderTint)
                }
            }
                .frame(width: isChildFolder ? 12 : 22, height: 22)
                .overlay(alignment: .topTrailing) {
                    // Attention only — the green "busy" dot was an old
                    // design; the desktop has no busy marker on project rows.
                    if hasAttentionSession {
                        Circle()
                            .fill(IOSSidebarTheme.attention)
                            .frame(width: 6, height: 6)
                            .background(
                                Circle()
                                    .fill(IOSSidebarTheme.attention.opacity(0.20))
                                    .frame(width: 14, height: 14)
                            )
                            .padding(.trailing, -1)
                    }
                }

            HStack(spacing: 4) {
                Text(project.name)
                    .font(.system(size: 14, weight: .medium))
                    .foregroundStyle(
                        project.isGroup == true
                            ? IOSSidebarTheme.foreground
                            : IOSSidebarTheme.foreground.opacity(0.62)
                    )
                    .lineLimit(1)
                    .truncationMode(.tail)

                // Plain groups use the same context-menu-only pinning model
                // as Sessions. Keep its passive mark inline with the label.
                if project.pinned == true {
                    SharedChromeIconView(icon: .pin, size: 11)
                        .foregroundStyle(IOSSidebarTheme.mutedForeground)
                        .fixedSize()
                        .accessibilityHidden(true)
                }
            }
            .layoutPriority(1)

            if let branch = project.worktreeBranch {
                HStack(spacing: 3) {
                    SharedChromeIconView(icon: .branch, size: 12)
                    if branch != project.name {
                        Text(branch)
                            .font(.system(size: 10, design: .monospaced))
                            .lineLimit(1)
                            .truncationMode(.tail)
                            .frame(maxWidth: 104, alignment: .leading)
                    }
                }
                .foregroundStyle(IOSSidebarTheme.mutedForeground.opacity(0.55))
            }

            Spacer(minLength: 4)

            if canCreateSession {
                Button(action: onAdd) {
                    SharedChromeIconView(icon: .plus, size: 17)
                        .foregroundStyle(IOSSidebarTheme.mutedForeground)
                        .frame(width: 28, height: 28)
                        .background(IOSSidebarTheme.hoverRow.opacity(0.44), in: RoundedRectangle(cornerRadius: 9, style: .continuous))
                }
                .buttonStyle(.plain)
                .accessibilityLabel("New session")
            }
        }
        .padding(EdgeInsets(
            top: 2,
            leading: isChildFolder
                // 18 + chevron 12 + spacing 7 = the 37pt text column used
                // by a normal session under this parent project.
                ? 18 + CGFloat(max(0, depth - 1)) * 14
                : 7 + CGFloat(depth) * 14,
            bottom: 2,
            trailing: 7
        ))
        .frame(minHeight: 32)
        .contentShape(Rectangle())
        .onTapGesture(perform: onToggle)
        .onLongPressGesture {
            onOrganize?()
        }
        .accessibilityLabel(
            "\(project.name), \(isExpanded ? "expanded" : "collapsed")"
                + (project.pinned == true ? ", pinned" : "")
        )
        .accessibilityHint(onOrganize == nil ? "" : "Hold to rename, sort, or color this folder")
    }

}

private extension View {
    /// Selected session-row background: real Liquid Glass on iOS 26 (matching
    /// the desktop app's selected row), the flat `activeRow` wash otherwise.
    /// Both are washed toward the Host's workspace tint like the desktop's
    /// `SelectedRowGlass`. Unselected rows get no background.
    @ViewBuilder
    func selectedRowBackground(_ selected: Bool, tintHue: Double?) -> some View {
        if #available(iOS 26.0, *), selected {
            glassEffect(
                .regular.tint(IOSSidebarTheme.activeRow(tintHue: tintHue)),
                in: RoundedRectangle(cornerRadius: 9, style: .continuous)
            )
        } else {
            background(
                RoundedRectangle(cornerRadius: 9, style: .continuous)
                    .fill(selected ? IOSSidebarTheme.activeRow(tintHue: tintHue) : .clear)
            )
        }
    }
}

private struct MacStyleSessionRow: View {
    let session: RemoteSessionSummary
    let project: RemoteProjectSummary?
    let selected: Bool
    var depth = 0
    /// An attached member of the representative Session's pane group. The
    /// phone still opens it as a normal one-terminal screen; this only changes
    /// its sidebar placement.
    var paneChild = false
    /// Pending MCP approval for this row, or a Host-reported blocked
    /// activity. Wins over the working spinner so the yellow badge is the
    /// findability path while the in-session prompt waits.
    var needsAttention = false
    /// The active Host's workspace tint hue — washes the selected chip.
    var tintHue: Double? = nil
    /// Long-press ("deep press") the row to open the rename/pin sheet for this
    /// session — the same sheet a long-press on the terminal title opens.
    var onOrganize: (() -> Void)? = nil
    /// Trailing (tap) action — kept last so existing trailing-closure call
    /// sites bind to it, not `onOrganize`.
    let onSelect: () -> Void

    var body: some View {
        rowContent
            .onLongPressGesture {
                onOrganize?()
            }
            .accessibilityLabel(
                "\(session.title), \(project?.name ?? "No project")"
                    + (session.pinned && !session.archived ? ", pinned" : "")
                    + (paneChild ? ", pane member" : "")
            )
            .accessibilityHint(onOrganize == nil ? "" : "Hold to organize this session")
    }

    private var rowContent: some View {
        HStack(spacing: 8) {
            leadingSlot
                .frame(width: 18, height: 18)

            HStack(spacing: 4) {
                // Keep the ordinary status column and row indentation. The
                // pane marker belongs to the label, not out in the gutter.
                if paneChild {
                    Text("└")
                        .font(.system(size: 12, weight: .regular, design: .monospaced))
                        .foregroundStyle(IOSSidebarTheme.mutedForeground.opacity(0.7))
                        .fixedSize()
                        .padding(.trailing, 4)
                        .accessibilityHidden(true)
                }

                Text(session.title)
                    .font(.system(size: 14, weight: .regular))
                    .foregroundStyle(session.status == .exited ? IOSSidebarTheme.mutedForeground : IOSSidebarTheme.foreground)
                    .lineLimit(1)
                    .truncationMode(.tail)

                // Pinning is context-menu / organize-sheet-only, just like
                // desktop. A pinned Session keeps this passive mark directly
                // beside its label; unpinned rows gain no extra chrome.
                if session.pinned && !session.archived {
                    SharedChromeIconView(icon: .pin, size: 11)
                        .foregroundStyle(IOSSidebarTheme.mutedForeground)
                        .fixedSize()
                        .accessibilityHidden(true)
                }
            }
            .layoutPriority(1)

            Spacer(minLength: 4)

            HStack(spacing: 4) {
                // Notify-when-done is on for this session (same flag the
                // organize sheet toggles) — mirrors the pin treatment.
                if session.notifyWhenDone {
                    SharedChromeIconView(icon: .bell, size: 11)
                        .foregroundStyle(IOSSidebarTheme.mutedForeground)
                }
                // TimelineView keeps the relative age ticking: with the poll
                // equality gate in place, an idle fleet never re-renders, so
                // a plain Date()-at-render label froze at "now"/"5m" forever.
                TimelineView(.periodic(from: .now, by: 60)) { context in
                    Text(meta(at: context.date))
                        .font(.system(size: 10, weight: selected ? .medium : .regular))
                        .foregroundStyle(IOSSidebarTheme.mutedForeground)
                        .frame(minWidth: 24, alignment: .trailing)
                }

                // Agent-CLI mark right of the date, matching the desktop
                // sidebar's SessionCommandIcon (12pt in a fixed 14×14 slot).
                SharedToolIconView(
                    providerID: session.presentationProviderID,
                    command: session.command,
                    size: 12
                )
                    .opacity(0.82)
                    .frame(width: 14, height: 14)
                    .padding(.leading, 1)
            }
        }
        .opacity(session.status == .exited ? 0.82 : 1)
        // Align the session LABEL with the parent project/folder NAME: indent
        // at the parent's depth (depth-1) and offset by (projectLeading 7 +
        // folderIconFrame 22 − sessionSlotFrame 18) = 11, so the text columns
        // line up rather than the session sitting one level deeper.
        .padding(EdgeInsets(
            top: 2,
            leading: 11 + CGFloat(max(depth - 1, 0)) * 14,
            bottom: 2,
            trailing: 9
        ))
        .frame(minHeight: 32)
        .selectedRowBackground(selected, tintHue: tintHue)
        .contentShape(Rectangle())
        .onTapGesture(perform: onSelect)
    }

    // All status indicators share this one leading column so they line up
    // vertically across rows: blocked/MCP-approval → attention dot, working
    // → spinner, settled-unread → the blue "unread" dot. Precedence:
    // attention > work > unread, matching the desktop MCP overlay.
    @ViewBuilder
    private var leadingSlot: some View {
        if session.status == .running
            && (needsAttention || session.activity == .blocked) {
            Circle()
                .fill(IOSSidebarTheme.attention)
                .frame(width: 6, height: 6)
                .background(
                    Circle()
                        .fill(IOSSidebarTheme.attention.opacity(0.20))
                        .frame(width: 14, height: 14)
                )
        } else if session.status == .running && session.activity == .working {
            TitlebarBrailleSpinner(color: IOSSidebarTheme.toolSpinnerColor(for: session))
                .scaleEffect(0.82)
        } else if session.unread {
            Circle()
                .fill(IOSSidebarTheme.unread)
                .frame(width: 7, height: 7)
        } else {
            Color.clear
        }
    }

    private func meta(at date: Date) -> String {
        if needsAttention || session.activity == .blocked { return "blocked" }
        return RelativeAge.shortString(
            fromUnixMs: session.updatedAtUnixMs ?? session.createdAtUnixMs,
            at: date
        )
    }
}


enum RelativeAge {
    static func shortString(fromUnixMs unixMs: Int64, at date: Date = Date()) -> String {
        let age = max(0, date.timeIntervalSince1970 - TimeInterval(unixMs) / 1000)
        if age < 60 { return "now" }
        if age < 3600 { return "\(Int(age / 60))m" }
        if age < 86_400 { return "\(Int(age / 3600))h" }
        return "\(Int(age / 86_400))d"
    }
}

extension Color {
    init(hex: UInt32, opacity: Double = 1) {
        self.init(
            .sRGB,
            red: Double((hex >> 16) & 0xFF) / 255,
            green: Double((hex >> 8) & 0xFF) / 255,
            blue: Double(hex & 0xFF) / 255,
            opacity: opacity
        )
    }
}

struct ActivityPill: View {
    let state: RemoteActivityState
    let status: RemoteSessionStatus

    var body: some View {
        Text(label)
            .font(.caption2.weight(.semibold))
            .foregroundStyle(color)
            .padding(.horizontal, 7)
            .padding(.vertical, 4)
            .background(color.opacity(0.12), in: Capsule())
    }

    private var label: String {
        if status == .exited { return "Done" }
        switch state {
        case .blocked: return "Blocked"
        case .working: return "Working"
        case .done: return "Done"
        case .starting: return "Starting"
        case .idle: return "Idle"
        case .unknown: return "Start"
        }
    }

    private var color: Color {
        if status == .exited { return .secondary }
        switch state {
        case .blocked: return .orange
        case .working: return .blue
        case .done: return .green
        case .starting: return .purple
        case .idle: return .secondary
        case .unknown: return .gray
        }
    }
}
