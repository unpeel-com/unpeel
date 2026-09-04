//
//  RemoteHostWorkspaceView.swift
//  UnpeelNative
//
//  Remote-scope terminal hosting and connection presentation. The old
//  parallel remote sidebar/content hierarchy is gone (design decision
//  2026-08-13: remote scope is the SAME UI) — the normal SidebarView and
//  ContentArea render remote state through UnpeelStore's display projection.
//  What remains here is the transport-level seam those shared views mount:
//  the runtime-owned in-memory Ghostty pane host, the connection banners,
//  and the connecting/empty states.
//

import AppKit
import SwiftUI
import UnpeelShared

// MARK: - Remote terminal mount (used by ContentArea's workspace pane)

/// Hosts the runtime's in-memory VT pane for the selected remote Session
/// inside the normal content chrome. This is the ONLY remote/local branch in
/// the content area, and it is a byte-transport choice: the surrounding
/// titlebar, banners, and empty states are the shared ones.
struct RemoteScopeTerminalMount: View {
    @ObservedObject var store: UnpeelStore
    let session: SessionEntry
    let backgroundColor: NSColor
    var isActive: Bool = true
    var onActivate: (() -> Void)? = nil

    @ObservedObject private var runtime: RemoteHostRuntime

    init(
        store: UnpeelStore,
        session: SessionEntry,
        backgroundColor: NSColor,
        isActive: Bool = true,
        onActivate: (() -> Void)? = nil
    ) {
        self.store = store
        self.session = session
        self.backgroundColor = backgroundColor
        self.isActive = isActive
        self.onActivate = onActivate
        _runtime = ObservedObject(wrappedValue: store.remoteHostRuntime)
    }

    var body: some View {
        if let pane = runtime.terminalPane(
            for: session.id,
            style: terminalPaneStyle,
            workingDirectory: session.cwd,
            onCommandClick: { match, path in
                store.openClickedFile(match, path: path, fromSessionID: session.id)
            }
        ) {
            RemoteTerminalPaneHostView(
                pane: pane,
                backgroundColor: backgroundColor,
                isActive: isActive,
                onActivate: onActivate,
                // File/image drops paste local paths — valid exactly when the
                // scoped session runs on THIS machine (local workspaces).
                fileDropsEnabled: store.selectedHostScope.isLocalMachine,
                dropStabilizeHome: store.scopedStateHome,
                // A TRUE remote Host receives image content over the phone's
                // upload operation and the returned Host path is pasted —
                // capability-gated, never probed.
                remoteUploader: !store.selectedHostScope.isLocalMachine
                    && runtime.supportsHostOperation(
                        RemoteHostRuntime.HostOperation.artifactUpload
                    )
                    ? { [weak runtime] contentType, bytes in
                        guard let runtime else {
                            throw RemoteHostVerbError(
                                operation: "attachment upload",
                                message: "The Host connection went away.",
                                outcomeIsUnknown: false
                            )
                        }
                        return try await runtime.uploadAttachment(
                            sessionID: session.id,
                            contentType: contentType,
                            bytes: bytes
                        )
                    }
                    : nil
            )
        } else {
            RemoteTerminalPreparingView(
                sessionTitle: session.label,
                state: runtime.connectionState
            )
        }
    }

    /// The Host resolves provider-specific terminal chrome; apply that value
    /// directly rather than reading provider configuration on the Controller.
    private var terminalPaneStyle: TerminalPaneStyle {
        var style = TerminalPaneStyle.resolved(
            runtimeID: session.activeRuntimeID,
            command: session.presentationCommand
        )
        guard let color = store.remoteTerminalBackgroundColor(for: session.id) else {
            return style
        }
        let rgb = color.usingColorSpace(.sRGB) ?? color
        let value = String(
            format: "#%02X%02X%02X",
            Int(rgb.redComponent * 255),
            Int(rgb.greenComponent * 255),
            Int(rgb.blueComponent * 255)
        )
        style.light.background = value
        style.dark.background = value
        return style
    }
}

/// Sidebar empty-state while a remote Host has no projects yet (connecting,
/// offline, or a genuinely empty Host). The local counterpart offers Add
/// Project — a Controller-local verb — so this presents connection state
/// instead; a genuinely empty, loaded workspace says so honestly rather
/// than pretending to load forever.
struct RemoteScopeEmptySidebarView: View {
    let hostName: String
    let state: RemoteHostConnectionState
    /// True once a bootstrap for this scope has been accepted — the sidebar
    /// being empty then means the workspace HAS no projects, not that they
    /// are still on the way.
    var hasLoadedSnapshot: Bool = false
    private var presentation: RemoteConnectionPresentation {
        .init(state: state, hasSnapshot: false)
    }

    private var isConnectedAndEmpty: Bool {
        if case .connected = state { return hasLoadedSnapshot }
        return false
    }

    /// Loading states render as a BLANK sidebar with one small centered
    /// spinner (`SidebarLoadingPlaceholder`, the same treatment the
    /// carousel's never-reached-host page uses, so a swipe commit into a
    /// still-connecting Host reads as one continuous quiet load):
    /// connecting/reconnecting with nothing to show yet, and the brief
    /// connected-but-projects-still-on-the-way gap. Error states (offline,
    /// repair, incompatible) and a genuinely empty Host keep the explicit
    /// iconography — a spinner there would promise progress that is not
    /// happening.
    private var showsLoadingSpinner: Bool {
        if presentation.showsProgress { return true }
        if case .connected = state { return !hasLoadedSnapshot }
        return false
    }

    var body: some View {
        if showsLoadingSpinner {
            SidebarLoadingPlaceholder()
        } else {
            VStack(spacing: 11) {
                Image(systemName: emptyStateIcon)
                    .font(.system(size: 24, weight: .light))
                    .foregroundStyle(presentation.tint)
                Text(presentation.shortLabel == "Connected" ? hostName : presentation.shortLabel)
                    .font(.system(size: 13, weight: .semibold))
                    .foregroundStyle(Theme.foreground)
                if isConnectedAndEmpty {
                    Text("No projects yet. Projects added on \(hostName) appear here.")
                        .font(.system(size: 11))
                        .foregroundStyle(Theme.mutedForeground)
                        .multilineTextAlignment(.center)
                        .lineLimit(4)
                } else {
                    Text(presentation.message ?? "Loading Sessions from \(hostName)…")
                        .font(.system(size: 11))
                        .foregroundStyle(Theme.mutedForeground)
                        .multilineTextAlignment(.center)
                        .lineLimit(4)
                }
            }
            .padding(.horizontal, 20)
            .frame(maxWidth: .infinity, maxHeight: .infinity)
        }
    }

    private var emptyStateIcon: String {
        switch state {
        case .repairRequired: return "exclamationmark.shield"
        case .incompatible: return "exclamationmark.triangle"
        case .failed: return "wifi.slash"
        default: return "server.rack"
        }
    }
}

// MARK: - AppKit pane host

/// The only AppKit bridge in the remote workspace. It can attach only the
/// runtime-owned in-memory pane type; there is no command, working directory,
/// attach binary, Local SurfaceCache, or filesystem callback in this path.
struct RemoteTerminalPaneHostView: NSViewRepresentable {
    let pane: RemoteGhosttyTerminalPane
    let backgroundColor: NSColor
    var isActive: Bool = true
    var onActivate: (() -> Void)? = nil
    /// Local-machine scopes accept file/image drops (pasted as local paths);
    /// true remote Hosts upload image content via `artifact.upload` instead.
    var fileDropsEnabled: Bool = false
    var dropStabilizeHome: URL? = nil
    var remoteUploader: ((String, Data) async throws -> String)? = nil

    @MainActor
    final class SwapContainer: NSView {
        private(set) weak var attachedPane: RemoteGhosttyTerminalPane?
        var backgroundColor = Theme.terminalBackgroundNSColor {
            didSet { needsDisplay = true }
        }
        var isActive = false
        var onActivate: (() -> Void)?

        // Honest opacity: translucent terminals swap the fill for .clear.
        override var isOpaque: Bool { backgroundColor.alphaComponent >= 1 }
        override var wantsUpdateLayer: Bool { true }

        override func updateLayer() {
            layer?.backgroundColor = backgroundColor.cgColor
        }

        override func layout() {
            super.layout()
            guard let pane = attachedPane, hosts(pane) else { return }
            pane.frame = bounds
        }

        override func hitTest(_ point: NSPoint) -> NSView? {
            let hit = super.hitTest(point)
            guard hit != nil,
                  let event = window?.currentEvent ?? NSApp.currentEvent,
                  event.type == .leftMouseDown
                    || event.type == .rightMouseDown
                    || event.type == .otherMouseDown
            else { return hit }
            onActivate?()
            return hit
        }

        @discardableResult
        func attach(_ pane: RemoteGhosttyTerminalPane) -> Bool {
            // If another container reparented this retained pane, a late
            // update from the former owner must not steal it back.
            guard attachedPane !== pane else { return false }
            detachPane()
            pane.removeFromSuperview()
            pane.translatesAutoresizingMaskIntoConstraints = true
            pane.autoresizingMask = [.width, .height]
            pane.frame = bounds
            addSubview(pane)
            attachedPane = pane
            pane.setPresentationEnabled(true)
            pane.frame = bounds
            return true
        }

        func hosts(_ pane: RemoteGhosttyTerminalPane) -> Bool {
            attachedPane === pane && pane.superview === self
        }

        func detachPane() {
            guard let pane = attachedPane else { return }
            attachedPane = nil
            // A retained pane may already have been reparented into a
            // replacement representable. A stale SwiftUI dismantle owns only
            // this container, never whichever container now hosts the pane.
            guard pane.superview === self else { return }
            pane.setPresentationEnabled(false)
            pane.removeFromSuperview()
        }
    }

    /// Under-surface backstop rule shared with the local SwapContainer:
    /// translucent terminals let the Ghostty surface carry the only canvas
    /// paint.
    private var effectiveBackground: NSColor {
        TransparencyModel.shared.surfaceOpacity < 1 ? .clear : backgroundColor
    }

    func makeNSView(context _: Context) -> SwapContainer {
        let container = SwapContainer()
        container.wantsLayer = true
        container.backgroundColor = effectiveBackground
        return container
    }

    func updateNSView(_ container: SwapContainer, context _: Context) {
        let fill = effectiveBackground
        container.backgroundColor = fill
        container.layer?.backgroundColor = fill.cgColor
        let becameActive = isActive && !container.isActive
        container.isActive = isActive
        container.onActivate = onActivate
        pane.fileDropsEnabled = fileDropsEnabled
        pane.dropStabilizeHome = dropStabilizeHome ?? LaunchConfig.unpeelDir
        pane.remoteUploader = remoteUploader
        let attached = container.attach(pane)

        if attached {
            pane.renderNow()
        }

        if isActive,
           (attached || becameActive),
           container.hosts(pane),
           container.window != nil {
            pane.focus()
            pane.renderNow()
        }

        guard attached || becameActive else { return }
        DispatchQueue.main.async { [weak container, weak pane] in
            guard let container, let pane, container.hosts(pane) else { return }
            pane.refitNow()
            if container.isActive {
                pane.focus()
                pane.renderNow()
            }
        }
    }

    static func dismantleNSView(_ container: SwapContainer, coordinator _: ()) {
        container.detachPane()
    }
}

// MARK: - Connection presentation (shared with ContentArea banners)

/// Pure visibility policy for the banner drawn over retained Host content.
/// The scheduler lives in `ContentArea`; keeping the state matrix here makes
/// the no-flash behavior deterministic and independently testable.
enum RemoteContentBannerPolicy {
    static func shouldScheduleConnectingDelay(
        state: RemoteHostConnectionState,
        hasSnapshot: Bool,
        isRemoteScope: Bool
    ) -> Bool {
        guard isRemoteScope, hasSnapshot else { return false }
        if case .connecting = state { return true }
        return false
    }

    static func allowsContentBanner(
        state: RemoteHostConnectionState,
        isRemoteScope: Bool,
        connectingDelayElapsed: Bool
    ) -> Bool {
        guard isRemoteScope else { return false }
        if case .connecting = state { return connectingDelayElapsed }
        return true
    }
}

struct RemoteConnectionPresentation {
    struct Banner {
        let icon: String
        let message: String
        let tint: Color
        let showsProgress: Bool
    }

    let shortLabel: String
    let message: String?
    let tint: Color
    let showsProgress: Bool
    let isStale: Bool
    let contentBanner: Banner?

    init(
        state: RemoteHostConnectionState,
        hasSnapshot: Bool,
        route: RemoteHostConnectionRoute? = nil
    ) {
        switch state {
        case .idle:
            shortLabel = "Disconnected"
            message = "This Host is not connected."
            tint = Theme.mutedForeground
            showsProgress = false
            isStale = hasSnapshot
            contentBanner = hasSnapshot
                ? Banner(
                    icon: "wifi.slash",
                    message: "Disconnected — showing the last known Host state.",
                    tint: Theme.mutedForeground,
                    showsProgress: false
                )
                : nil
        case .connecting:
            shortLabel = "Connecting…"
            message = "Connecting to this Host."
            tint = Theme.accent
            showsProgress = true
            isStale = hasSnapshot
            contentBanner = hasSnapshot
                ? Banner(
                    icon: "arrow.clockwise",
                    message: "Connecting — showing the last known Host state.",
                    tint: Theme.accent,
                    showsProgress: true
                )
                : nil
        case .connected:
            shortLabel = route?.shortLabel ?? "Connected"
            message = nil
            tint = Theme.accent
            showsProgress = false
            isStale = false
            contentBanner = nil
        case let .reconnecting(message):
            shortLabel = "Reconnecting…"
            self.message = message
            tint = Theme.attention
            showsProgress = true
            isStale = hasSnapshot
            contentBanner = Banner(
                icon: "arrow.clockwise",
                message: "Connection interrupted — reconnecting while the last known state stays visible.",
                tint: Theme.attention,
                showsProgress: true
            )
        case let .repairRequired(message):
            shortLabel = "Pair again"
            self.message = message
            tint = Theme.danger
            showsProgress = false
            isStale = hasSnapshot
            contentBanner = Banner(
                icon: "exclamationmark.shield",
                message: "This pairing is no longer valid. Pair the Host again before sending input.",
                tint: Theme.danger,
                showsProgress: false
            )
        case let .incompatible(message):
            shortLabel = "Update required"
            self.message = message
            tint = Theme.danger
            showsProgress = false
            isStale = hasSnapshot
            contentBanner = Banner(
                icon: "exclamationmark.triangle",
                message: "This Host uses an incompatible protocol. Update Unpeel on both devices.",
                tint: Theme.danger,
                showsProgress: false
            )
        case let .failed(message):
            shortLabel = "Offline"
            self.message = message
            tint = Theme.danger
            showsProgress = false
            isStale = hasSnapshot
            contentBanner = hasSnapshot
                ? Banner(
                    icon: "wifi.slash",
                    message: "Host offline — showing the last known state.",
                    tint: Theme.danger,
                    showsProgress: false
                )
                : nil
        }
    }
}

struct RemoteHostConnectionBanner: View {
    let banner: RemoteConnectionPresentation.Banner

    var body: some View {
        HStack(spacing: 8) {
            if banner.showsProgress {
                ProgressView()
                    .controlSize(.small)
                    .tint(banner.tint)
            } else {
                Image(systemName: banner.icon)
                    .font(.system(size: 11, weight: .semibold))
                    .foregroundStyle(banner.tint)
            }
            Text(banner.message)
                .font(.system(size: 11, weight: .medium))
                .foregroundStyle(Theme.mutedForeground)
                .lineLimit(2)
            Spacer(minLength: 0)
        }
        .padding(.horizontal, 12)
        .frame(minHeight: 34)
        .background(banner.tint.opacity(0.07))
        .overlay(alignment: .bottom) {
            Rectangle()
                .fill(Theme.resizerLine)
                .frame(height: 1)
        }
    }
}

struct RemoteTerminalPreparingView: View {
    let sessionTitle: String
    let state: RemoteHostConnectionState

    var body: some View {
        VStack(spacing: 10) {
            if case .repairRequired = state {
                Image(systemName: "exclamationmark.shield")
                    .font(.system(size: 25, weight: .light))
                    .foregroundStyle(Theme.danger)
            } else if case .incompatible = state {
                Image(systemName: "exclamationmark.triangle")
                    .font(.system(size: 25, weight: .light))
                    .foregroundStyle(Theme.danger)
            } else {
                ProgressView()
                    .controlSize(.small)
            }
            Text(sessionTitle.isEmpty ? "Preparing terminal…" : sessionTitle)
                .font(.system(size: 12, weight: .medium))
                .foregroundStyle(Theme.mutedForeground)
        }
        .frame(maxWidth: .infinity, maxHeight: .infinity)
    }
}

// MARK: - Host service status (Local scope)

/// Small non-modal status for the bundled `unpeel serve` service. A healthy
/// launch never shows it: "starting" appears only when the service has not
/// answered for a while, and "unavailable" offers a Retry that forces one
/// relaunch past the cooldown. The disk-seeded sidebar stays visible either
/// way; the app never falls back to hosting Sessions itself.
struct HostServiceStatusBanner: View {
    @ObservedObject private var service = HostServiceManager.shared
    @State private var startingVisible = false
    @State private var startingGeneration = 0
    /// How long "starting" may stay silent before the banner appears.
    static let startingDelay: TimeInterval = 2.5

    var body: some View {
        Group {
            switch service.serviceState {
            case .live:
                EmptyView()
            case .starting:
                if startingVisible {
                    row(
                        icon: nil,
                        message: "Host service starting…",
                        tint: Theme.accent,
                        showsProgress: true,
                        retry: false
                    )
                }
            case .unavailable:
                row(
                    icon: "exclamationmark.triangle",
                    message: "Host service unavailable — terminals cannot be created until it answers.",
                    tint: Theme.danger,
                    showsProgress: false,
                    retry: true
                )
            }
        }
        .onAppear { refreshStartingDelay() }
        .onChange(of: service.serviceState) { _ in refreshStartingDelay() }
    }

    private func refreshStartingDelay() {
        startingGeneration &+= 1
        let generation = startingGeneration
        startingVisible = false
        guard case .starting = service.serviceState else { return }
        DispatchQueue.main.asyncAfter(deadline: .now() + Self.startingDelay) {
            guard startingGeneration == generation,
                  case .starting = service.serviceState
            else { return }
            startingVisible = true
        }
    }

    private func row(
        icon: String?,
        message: String,
        tint: Color,
        showsProgress: Bool,
        retry: Bool
    ) -> some View {
        HStack(spacing: 8) {
            if showsProgress {
                ProgressView()
                    .controlSize(.small)
                    .tint(tint)
            } else if let icon {
                Image(systemName: icon)
                    .font(.system(size: 11, weight: .semibold))
                    .foregroundStyle(tint)
            }
            Text(message)
                .font(.system(size: 11, weight: .medium))
                .foregroundStyle(Theme.mutedForeground)
                .lineLimit(2)
            Spacer(minLength: 0)
            if retry {
                Button("Retry") { service.retryNow() }
                    .buttonStyle(.bordered)
                    .controlSize(.small)
            }
        }
        .padding(.horizontal, 12)
        .frame(minHeight: 34)
        .background(tint.opacity(0.07))
        .overlay(alignment: .bottom) {
            Rectangle()
                .fill(Theme.resizerLine)
                .frame(height: 1)
        }
    }
}
