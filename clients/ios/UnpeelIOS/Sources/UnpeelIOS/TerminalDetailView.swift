import SwiftUI
import UnpeelShared
#if os(iOS)
import UIKit
#endif

enum TerminalChrome {
    /// The near-black frame behind terminal cards, matching the desktop
    /// app's Background plane (Linear-style hierarchy, 2026-09-01: the
    /// frame sits BELOW the slightly lighter Surface the terminal keeps).
    static let frameBackgroundHex: UInt32 = 0x121314
    static let backgroundHex: UInt32 = 0x1A1B1D
    static let background = Color(hex: backgroundHex)

    /// A rounder mobile radius follows the iPhone enclosure more naturally
    /// than the desktop's tighter 10pt card.
    static let cardCornerRadius: CGFloat = 18

    static func frameBackground(tintHue: Double?) -> Color {
        hostTinted(hex: frameBackgroundHex, tintHue: tintHue)
    }

    /// The app background washed toward the active Host's tint hue
    /// (`RemoteBootstrapSnapshot.hostTintHue`). Nil returns the shipped
    /// neutral color untouched, so older Hosts keep identical bytes.
    static func background(tintHue: Double?) -> Color {
        hostTinted(hex: backgroundHex, tintHue: tintHue)
    }

    /// Host-tint wash over a neutral chrome hex — the same algorithm as the
    /// Mac's `Theme.appTinted`: keep the base color's brightness and alpha,
    /// push its hue to the tint with a brightness-scaled saturation, so the
    /// phone chrome reads as the same gray, warmer/cooler, matching the
    /// workspace it controls.
    static func hostTinted(
        hex: UInt32, opacity: Double = 1, tintHue: Double?
    ) -> Color {
        guard let tintHue else { return Color(hex: hex, opacity: opacity) }
        let washed = hostTintedComponents(hex: hex, tintHue: tintHue)
        return Color(
            .sRGB,
            red: washed.red,
            green: washed.green,
            blue: washed.blue,
            opacity: opacity
        )
    }

    /// The same wash in 0xRRGGBB form, for the Ghostty theme path (the
    /// renderer paints hex strings directly, so the default canvas must
    /// track the tinted chrome exactly). Nil hue returns the input hex.
    static func hostTintedHex(_ hex: UInt32, tintHue: Double?) -> UInt32 {
        guard let tintHue else { return hex }
        let washed = hostTintedComponents(hex: hex, tintHue: tintHue)
        func channel(_ value: Double) -> UInt32 {
            UInt32((value * 255).rounded()) & 0xFF
        }
        return channel(washed.red) << 16
            | channel(washed.green) << 8
            | channel(washed.blue)
    }

    /// RGB → HSB, wash the saturation, then HSB → sRGB by hand — mirroring
    /// the Mac's `Theme.appTinted` + `srgbColor` so both ends produce the
    /// same bytes for the same base color and hue.
    private static func hostTintedComponents(
        hex: UInt32, tintHue: Double
    ) -> (red: Double, green: Double, blue: Double) {
        let red = Double((hex >> 16) & 0xFF) / 255
        let green = Double((hex >> 8) & 0xFF) / 255
        let blue = Double(hex & 0xFF) / 255
        let brightness = max(red, green, blue)
        let delta = brightness - min(red, green, blue)
        let saturation = brightness == 0 ? 0 : delta / brightness
        // Mac twin (Theme.appTinted): steeper at the bright end so
        // light-mode paper reads as white (~2.5% cast at b=1, 2026-08-23);
        // keep in lockstep.
        let wash = max(saturation, min(0.5, 0.615 - 0.59 * brightness))
        let hue = tintHue / 360
        let h = (hue - hue.rounded(.down)) * 6
        let f = h - h.rounded(.down)
        let p = brightness * (1 - wash)
        let q = brightness * (1 - wash * f)
        let t = brightness * (1 - wash * (1 - f))
        switch Int(h) % 6 {
        case 0: return (brightness, t, p)
        case 1: return (q, brightness, p)
        case 2: return (p, brightness, t)
        case 3: return (p, q, brightness)
        case 4: return (t, p, brightness)
        default: return (brightness, p, q)
        }
    }
}

/// The single sheet the terminal top bar can present. Modeled as one
/// `.sheet(item:)` because stacking multiple `.sheet(isPresented:)` modifiers
/// on the same view silently breaks presentation in SwiftUI (only one wins) —
/// which is what was stopping the bell from opening.
public enum TopBarSheet: String, Identifiable {
    case activity
    case organize
    /// Folder organize sheet for `store.organizeTargetProjectID` (sidebar
    /// project/group long-press): rename, sort, color, archive library.
    case organizeProject
    case gallery
    case textSelection
    /// The archive library of `store.archiveLibraryProjectID` (opened from a
    /// project's organize sheet) — restore / restore-and-resume older sessions.
    case archive
    public var id: String { rawValue }
}

struct TerminalDetailView: View {
    var store: RemotePreviewStore
    @EnvironmentObject private var connection: RemoteConnectionStore

    /// Live open-drag peek distance. `@GestureState` auto-resets to nil the
    /// instant the gesture ends OR is cancelled — so the sidebar peek can never
    /// get stuck (which previously froze the app: a non-nil reveal made the
    /// drawer overlay swallow all touches). Mirrored into the store so the
    /// drawer overlay (a sibling view) can render the peek.
    @GestureState private var sidebarRevealGesture: CGFloat?

    /// Current detail width, used to keep the sidebar reveal hit region
    /// proportional across phone orientations and mobile window sizes.
    @State private var sidebarRevealScreenWidth: CGFloat = 0

    /// Any overlay owning the screen unfocuses the terminal (keyboard +
    /// accessory bar down) — driven as STATE into the surface, because
    /// responder-chain pokes race the focus re-assertion.
    private var overlayPresented: Bool {
        store.sessionsDrawerPresented
            || store.presetDrawerProjectID != nil
            || store.topBarSheet != nil
            || connection.pairingSheetPresented
            || store.selectedSessionID.flatMap(store.pendingApproval(forSessionID:)) != nil
    }

    /// Drives cache eviction for sessions that disappeared on the Mac.
    private var liveSessionIDs: Set<String> {
        Set(store.snapshot.sessions.map(\.id))
    }

    /// The title controls live on the lighter desktop-style frame. Provider
    /// terminal colors are now confined to the rounded terminal canvas card.
    private var frameBackground: Color {
        TerminalChrome.frameBackground(tintHue: store.hostTintHue)
    }

    var body: some View {
        ZStack {
            TerminalBackground(color: frameBackground)
            if let session = store.selectedSession {
                ZStack(alignment: .top) {
                    RemoteGhosttyTerminalSurface(
                        session: session,
                        client: connection.client,
                        connectionEpoch: connection.epoch,
                        topContentInset: SessionDetailLayout.terminalTopInset,
                        landscapeTopContentInset: SessionDetailLayout.terminalLandscapeTopInset,
                        bottomContentInset: SessionDetailLayout.terminalBottomInset,
                        suppressFocus: overlayPresented,
                        store: store
                    )
                        // Epoch in the identity: a renderer captures its
                        // client at creation, so (un)pairing must rebuild it.
                        .id("\(session.id)-\(connection.epoch)-terminal")
                        // Session switches ride the drawer-close animation's
                        // transaction; without .identity the id swap gets the
                        // default .opacity transition and the terminal text
                        // crossfades. Cached renderers already show the last
                        // frame instantly — the swap must be instant too.
                        .transition(.identity)
                        .frame(maxWidth: .infinity, maxHeight: .infinity)
                        #if os(iOS)
                        // Keyboard included: avoidance must not shove the
                        // whole surface up — the surface tracks the keyboard
                        // itself and shrinks its visible viewport instead.
                        .ignoresSafeArea(.container, edges: [.top, .bottom])
                        .ignoresSafeArea(.keyboard)
                        #endif

                    TerminalTopBar(
                        store: store,
                        session: session
                    )
                        .zIndex(4)

                    if let approval = store.pendingApproval(forSessionID: session.id) {
                        ApprovalPromptOverlay(store: store, approval: approval)
                            .zIndex(3)
                            .transition(.opacity)
                    }

                    if session.status == .exited,
                       session.capabilities?.restart ?? false {
                        ExitedSessionRestartBar(
                            restarting: store.isRestartingSelectedSession,
                            errorText: store.restartError,
                            onRestart: { store.restartSelectedSession() }
                        )
                        .frame(maxWidth: .infinity, maxHeight: .infinity, alignment: .bottom)
                        .padding(.bottom, SessionDetailLayout.terminalBottomInset + 12)
                        .zIndex(2)
                        .transition(.move(edge: .bottom).combined(with: .opacity))
                    }
                }
                .animation(.easeOut(duration: 0.22), value: session.status)
                .animation(
                    .easeOut(duration: 0.18),
                    value: store.pendingApproval(forSessionID: session.id)?.id
                )
            } else {
                EmptyTerminalState(store: store)
            }
        }
        #if os(iOS)
        // Keep the opening band proportional to the current screen width.
        .onGeometryChange(for: CGFloat.self) { proxy in
            proxy.size.width
        } action: { width in
            sidebarRevealScreenWidth = width
        }
        // Reveal the sessions sidebar by dragging rightward from the
        // leftmost 15% of the screen. Simultaneous + commit-on-release so
        // it never blocks terminal taps/scroll: it only fires when the
        // drag starts in that region, ends decisively rightward, and is
        // clearly horizontal.
        .simultaneousGesture(sidebarRevealDrag(screenWidth: sidebarRevealScreenWidth))
        // Mirror the auto-clearing peek into the store so the root drawer can
        // slide over this stationary terminal — and, crucially, so the peek
        // always returns to nil when the gesture ends/cancels.
        .onChange(of: sidebarRevealGesture) { newValue in
            guard store.sidebarDragReveal != newValue else { return }
            if newValue == nil, !store.sessionsDrawerPresented {
                withAnimation(.spring(response: 0.30, dampingFraction: 0.88)) {
                    store.sidebarDragReveal = nil
                }
            } else {
                store.sidebarDragReveal = newValue
            }
        }
        #endif
        .navigationBarBackButtonHidden()
        // Cached terminals for sessions killed on the Mac are dead weight
        // (their hosts are gone) — evict as the session list changes. Epoch
        // changes flush inside the cache lookup itself.
        .onChange(of: liveSessionIDs) { ids in
            TerminalSessionCache.shared.pruneMissingSessions(ids)
        }
        // The terminal chrome is always dark; without this the glass
        // buttons follow the SYSTEM scheme and render white in light mode.
        .environment(\.colorScheme, .dark)
        #if os(iOS)
        .toolbar(.hidden, for: .navigationBar)
        .ignoresSafeArea(.container, edges: .bottom)
        .ignoresSafeArea(.keyboard)
        #endif
    }

    #if os(iOS)
    /// How far in from the left edge a reveal drag may start.
    private static let sidebarRevealStartFraction: CGFloat = 0.15

    /// Pure gesture classification shared by the live update and release
    /// paths. Once terminal text selection has recognized the hold, movement
    /// belongs exclusively to its selection drag — never to the drawer.
    static func acceptsSidebarReveal(
        startLocation: CGPoint,
        translation: CGSize,
        screenWidth: CGFloat,
        pointerSelectionActive: Bool
    ) -> Bool {
        guard !pointerSelectionActive, screenWidth > 0 else { return false }
        let horizontal = abs(translation.width) > abs(translation.height) * 1.3
        return startLocation.x <= screenWidth * sidebarRevealStartFraction
            && horizontal
            && translation.width > 0
    }

    private func sidebarRevealDrag(screenWidth: CGFloat) -> some Gesture {
        // Match the drawer's global drag coordinates so the opening gesture
        // hands off without a coordinate-space jump.
        DragGesture(minimumDistance: 18, coordinateSpace: .global)
            .updating($sidebarRevealGesture) { value, state, _ in
                // Only a rightward, horizontal-dominant drag that started in the
                // left region peeks the drawer. Setting the @GestureState here
                // (not the store directly) is what guarantees the peek clears
                // when the gesture ends or is cancelled.
                guard !overlayPresented, !store.sessionsDrawerPresented else {
                    state = nil
                    return
                }
                if Self.acceptsSidebarReveal(
                    startLocation: value.startLocation,
                    translation: value.translation,
                    screenWidth: screenWidth,
                    pointerSelectionActive: store.terminalPointerSelectionActive
                ) {
                    state = value.translation.width
                } else {
                    state = nil
                }
            }
            .onEnded { value in
                // Decide commit straight from the final value (robust — no
                // dependence on the peek state, which may already be clearing).
                guard !overlayPresented, !store.sessionsDrawerPresented else { return }
                guard Self.acceptsSidebarReveal(
                    startLocation: value.startLocation,
                    translation: value.translation,
                    screenWidth: screenWidth,
                    pointerSelectionActive: store.terminalPointerSelectionActive
                )
                else { return }
                if value.translation.width > 90 || value.predictedEndTranslation.width > 220 {
                    dismissTerminalKeyboard()
                    withAnimation(.timingCurve(0.16, 1, 0.3, 1, duration: 0.28)) {
                        // Hand the live gesture directly to the settled open
                        // distance in the same transaction — no intermediate
                        // snap back to zero when @GestureState clears.
                        store.sidebarDragReveal = nil
                        store.sessionsDrawerPresented = true
                    }
                }
                // Cancel needs no work: the @GestureState reset clears the peek.
            }
    }
    #endif
}

/// Resign the terminal's first responder — hides the software keyboard AND
/// the extra-keys accessory bar. Call whenever an overlay (drawer, sheet,
/// popover) takes over the screen; the terminal's onFocusChange mirror keeps
/// its focus state consistent so nothing re-summons it.
@MainActor
func dismissTerminalKeyboard() {
    #if os(iOS)
    UIApplication.shared.sendAction(
        #selector(UIResponder.resignFirstResponder),
        to: nil,
        from: nil,
        for: nil
    )
    #endif
}

extension View {
    /// Round floating control in the system's own glass (Liquid Glass on
    /// iOS 26, material fallback): no hardcoded foreground, so icons follow
    /// the native tint; 44pt hit target around a 40pt circle.
    @ViewBuilder
    func iosCircularGlassControl() -> some View {
        if #available(iOS 26.0, macOS 26.0, *) {
            self
                .buttonStyle(.plain)
                .glassEffect(.regular.interactive(), in: Circle())
                .frame(width: 44, height: 44)
                .contentShape(Circle())
        } else {
            self
                .buttonStyle(.plain)
                .background(.ultraThinMaterial, in: Circle())
                .overlay(Circle().strokeBorder(.white.opacity(0.10)))
                .frame(width: 44, height: 44)
                .contentShape(Circle())
        }
    }

    /// Prominent action button in Liquid Glass on iOS 26 (`.glassProminent`),
    /// bordered-prominent fallback. `tint` drives the accent both ways.
    @ViewBuilder
    func liquidGlassProminentButton(tint: Color = .accentColor) -> some View {
        if #available(iOS 26.0, *) {
            self.buttonStyle(.glassProminent).tint(tint)
        } else {
            self.buttonStyle(.borderedProminent).tint(tint)
        }
    }

    /// Secondary action button in Liquid Glass on iOS 26 (`.glass`), bordered
    /// fallback.
    @ViewBuilder
    func liquidGlassButton() -> some View {
        if #available(iOS 26.0, *) {
            self.buttonStyle(.glass)
        } else {
            self.buttonStyle(.bordered)
        }
    }

    /// Liquid Glass capsule background (iOS 26), dark material fallback — for
    /// floating pills over the terminal.
    @ViewBuilder
    func liquidGlassPill() -> some View {
        if #available(iOS 26.0, *) {
            self.glassEffect(.regular, in: Capsule())
        } else {
            self
                .background(.black.opacity(0.55), in: Capsule())
                .overlay(Capsule().strokeBorder(.white.opacity(0.12)))
        }
    }

    @ViewBuilder
    func iosGlassControl(cornerRadius: CGFloat = 10, active: Bool = false) -> some View {
        if #available(iOS 26.0, macOS 26.0, *) {
            self
                .buttonStyle(.plain)
                .glassEffect(.regular, in: RoundedRectangle(cornerRadius: cornerRadius, style: .continuous))
        } else {
            self
                .buttonStyle(.plain)
                .background(
                    .ultraThinMaterial,
                    in: RoundedRectangle(cornerRadius: cornerRadius, style: .continuous)
                )
                .overlay(
                    RoundedRectangle(cornerRadius: cornerRadius, style: .continuous)
                        .strokeBorder(.white.opacity(active ? 0.18 : 0.10), lineWidth: 1)
                )
                .shadow(color: .black.opacity(active ? 0.24 : 0.18), radius: active ? 12 : 8, y: 4)
        }
    }
}

private enum SessionDetailLayout {
    // Keep an 8pt breathing gap below the title controls. The root no longer
    // adds vertical card padding, so this reserve is the sole top placement.
    static let terminalTopInset: CGFloat = 124
    static let terminalLandscapeTopInset: CGFloat = 72
    static let terminalBottomInset: CGFloat = 16
}

struct TerminalBackground: View {
    /// The session's resolved terminal background (opencode/grok theme) so the
    /// safe-area edges match too; default terminal color otherwise.
    var color: Color = TerminalChrome.background
    var body: some View {
        color.ignoresSafeArea()
    }
}

/// Shown at the bottom of the terminal when the selected session has exited.
/// Resume re-runs the session's command on the Mac with a resume flag
/// (title/pin/worktree/grants preserved) — the same action as the desktop
/// context menu's Resume verb for stopped sessions.
struct ExitedSessionRestartBar: View {
    let restarting: Bool
    var errorText: String? = nil
    let onRestart: () -> Void

    private var statusText: String {
        if restarting { return "Resuming…" }
        if let errorText, !errorText.isEmpty { return errorText }
        return "Session ended"
    }

    var body: some View {
        HStack(spacing: 12) {
            Image(systemName: errorText != nil && !restarting ? "exclamationmark.triangle.fill" : "stop.circle.fill")
                .font(.system(size: 15, weight: .semibold))
                .foregroundStyle(errorText != nil && !restarting ? Color.orange.opacity(0.9) : .white.opacity(0.6))
            Text(statusText)
                .font(.subheadline.weight(.medium))
                .foregroundStyle(.white.opacity(0.85))
                .lineLimit(2)
            Spacer(minLength: 8)
            Button(action: onRestart) {
                HStack(spacing: 6) {
                    if restarting {
                        ProgressView()
                            .controlSize(.small)
                            .tint(.white)
                    } else {
                        Image(systemName: "arrow.clockwise")
                            .font(.system(size: 13, weight: .bold))
                    }
                    Text("Resume")
                        .font(.subheadline.weight(.semibold))
                }
                .foregroundStyle(.white)
                .padding(.horizontal, 14)
                .padding(.vertical, 8)
                .background(Color.accentColor, in: Capsule())
            }
            .buttonStyle(.plain)
            .disabled(restarting)
            .opacity(restarting ? 0.7 : 1)
        }
        .padding(.leading, 16)
        .padding(.trailing, 8)
        .padding(.vertical, 8)
        .background(.ultraThinMaterial, in: Capsule())
        .overlay(Capsule().strokeBorder(.white.opacity(0.12)))
        .shadow(color: .black.opacity(0.28), radius: 14, y: 6)
        .padding(.horizontal, 16)
        .accessibilityElement(children: .combine)
        .accessibilityLabel(restarting ? "Resuming session" : "Session ended")
        .accessibilityHint("Resume re-runs this session and continues the conversation")
    }
}

struct TerminalTopBar: View {
    var store: RemotePreviewStore
    @EnvironmentObject private var connection: RemoteConnectionStore
    let session: RemoteSessionSummary
    /// Pulses the top-bar gallery control when a new browser/computer capture
    /// lands for this session.
    @State private var galleryPulse = false

    private static let captureArtifactKinds: Set<String> = ["screenshots", "computer"]

    var body: some View {
        ZStack {
            VStack(alignment: .center, spacing: 2) {
                HStack(spacing: 4) {
                    Text(session.title)
                        .font(.subheadline.weight(.semibold))
                        .foregroundStyle(.white)
                        .lineLimit(1)
                }
                HStack(spacing: 6) {
                    if let context = displayContext {
                        Text(context)
                    }
                    if let branch = displayBranch {
                        HStack(spacing: 2) {
                            Image(systemName: "arrow.triangle.branch")
                                .font(.system(size: 9, weight: .semibold))
                            Text(branch)
                        }
                    }
                }
                .font(.caption2)
                .foregroundStyle(.white.opacity(0.55))
                .lineLimit(1)
            }
            .multilineTextAlignment(.center)
            .frame(maxWidth: 160)
            .contentShape(Rectangle())
            .onLongPressGesture {
                dismissTerminalKeyboard()
                store.presentSessionOrganize(for: nil)
            }
            .accessibilityAddTraits(.isButton)
            .accessibilityHint("Hold to organize this session")

            HStack(spacing: 10) {
                Button {
                    store.showSessions()
                } label: {
                    // Same glass-gradient sidebar glyph as the desktop titlebar
                    // (shared SVG, rendered by SharedChromeIconView). The shared
                    // icon image opts out of hit testing (so it never blocks row
                    // taps), so the button label needs its own hittable shape or
                    // the button becomes untappable.
                    SharedChromeIconView(icon: .sidebarToggle, size: 18)
                        .frame(width: 40, height: 40)
                        .contentShape(Rectangle())
                }
                .iosCircularGlassControl()
                .accessibilityLabel("Open sessions")

                Spacer(minLength: 0)

                // The bell is always shown — not gated on there being
                // active/unread sessions (that made it vanish when nothing
                // was running). Spins while any agent works anywhere;
                // otherwise a plain bell. The gallery is agent chrome:
                // shells and App sessions produce no review artifacts, so
                // only agent sessions get the control (stable per session,
                // so it never flickers like activity gating would).
                HStack(spacing: 8) {
                    if session.isAgentSession {
                        Button {
                            dismissTerminalKeyboard()
                            galleryPulse = false
                            store.topBarSheet = .gallery
                        } label: {
                            SharedChromeIconView(icon: .gallery, size: 19)
                                .frame(width: 40, height: 40)
                                .contentShape(Rectangle())
                        }
                        .iosCircularGlassControl()
                        .scaleEffect(galleryPulse ? 1.08 : 1)
                        .animation(
                            .spring(response: 0.3, dampingFraction: 0.5),
                            value: galleryPulse
                        )
                        .accessibilityLabel("Session gallery")
                    }

                    TitlebarActivityButton(
                        spinning: !store.bellActiveSessions.isEmpty,
                        hasBlockers: !store.bellBlockedSessions.isEmpty,
                        action: {
                            dismissTerminalKeyboard()
                            store.topBarSheet = .activity
                        }
                    )
                }
            }
        }
        .padding(.horizontal, 12)
        .padding(.top, 10)
        .padding(.bottom, 8)
        // The bell/organize sheets are presented at the ROOT (see
        // UnpeelIOSRootView), driven by store.topBarSheet — a `.sheet` nested
        // over the Metal terminal surface doesn't present reliably.
        .task(id: session.id) { await watchForNewScreenshots() }
    }

    /// Lightweight metadata polling. The first sample establishes the floor;
    /// only a newer agent capture pulses the gallery control.
    @MainActor
    private func watchForNewScreenshots() async {
        // No gallery control to pulse for shells/App sessions — skip the
        // artifact polling entirely.
        guard session.isAgentSession else { return }
        var baseline: Int64 = -1
        while !Task.isCancelled {
            if let list = try? await store.client.browserArtifacts(sessionID: session.id) {
                let newest = list.artifacts
                    .filter { Self.captureArtifactKinds.contains($0.kind) }
                    .map(\.modifiedAtUnixMs)
                    .max() ?? 0
                if baseline < 0 {
                    baseline = newest
                } else if newest > baseline {
                    baseline = newest
                    galleryPulse = true
                    try? await Task.sleep(nanoseconds: 2_200_000_000)
                    galleryPulse = false
                }
            }
            try? await Task.sleep(nanoseconds: 5_000_000_000)
        }
    }

    /// Branch under the title, like the desktop titlebar: the session's
    /// worktree branch when it has one, else the project's live HEAD.
    private var displayBranch: String? {
        session.worktreeBranch
            ?? store.projectsByID[session.projectID]?.gitBranch
    }

    /// Project folder name under the title (where the session runs), falling
    /// back to the CLI type when the project is unknown.
    private var displayContext: String? {
        if let name = store.projectsByID[session.projectID]?.name, !name.isEmpty {
            return name
        }
        if let provider = session.presentationProviderID, !provider.isEmpty {
            return provider
        }
        return nil
    }
}

struct TitlebarActivityButton: View {
    let spinning: Bool
    let hasBlockers: Bool
    let action: () -> Void

    private var indicatorColor: Color {
        hasBlockers ? IOSSidebarTheme.attention : .primary
    }

    var body: some View {
        Button(action: action) {
            Group {
                if spinning {
                    TitlebarBrailleSpinner(color: indicatorColor)
                } else {
                    SharedChromeIconView(icon: .bell, size: 17)
                }
            }
            .foregroundStyle(indicatorColor)
            .frame(width: 40, height: 40)
            .overlay(alignment: .topTrailing) {
                if hasBlockers {
                    Circle()
                        .fill(IOSSidebarTheme.attention)
                        .frame(width: 7, height: 7)
                        .overlay {
                            Circle()
                                .stroke(TerminalChrome.background, lineWidth: 1.5)
                        }
                        .offset(x: -4, y: 4)
                }
            }
            // The shared icon opts out of hit testing — give the label its own
            // hittable shape so the button stays tappable.
            .contentShape(Rectangle())
        }
        .iosCircularGlassControl()
        .accessibilityLabel(
            hasBlockers
                ? "Show blocked sessions"
                : spinning ? "Show active sessions" : "Show recent sessions"
        )
    }
}

struct ActivitySessionsPanel: View {
    let blocked: [RemoteSessionSummary]
    let active: [RemoteSessionSummary]
    let recent: [RemoteSessionSummary]
    var projectsByID: [String: RemoteProjectSummary] = [:]
    let onSelect: (RemoteSessionSummary) -> Void

    var body: some View {
        ZStack {
            Color(hex: 0x16171C).ignoresSafeArea()
            ScrollView {
                VStack(alignment: .leading, spacing: 0) {
                    if blocked.isEmpty && active.isEmpty && recent.isEmpty {
                        Text("No active sessions")
                            .font(.system(size: 13, weight: .medium))
                            .foregroundStyle(.white.opacity(0.62))
                            .padding(18)
                    } else {
                        if !blocked.isEmpty {
                            ActivityPanelSectionTitle("Blocked")
                            ForEach(blocked) { session in
                                ActivityPanelRow(
                                    session: session,
                                    working: false,
                                    blocked: true,
                                    projectName: projectsByID[session.projectID]?.name
                                ) {
                                    onSelect(session)
                                }
                            }
                        }

                        if !active.isEmpty {
                            if !blocked.isEmpty {
                                activityDivider
                            }
                            ActivityPanelSectionTitle("Active")
                            ForEach(active) { session in
                                ActivityPanelRow(
                                    session: session,
                                    working: true,
                                    blocked: false,
                                    projectName: projectsByID[session.projectID]?.name
                                ) {
                                    onSelect(session)
                                }
                            }
                        }

                        if !recent.isEmpty {
                            if !blocked.isEmpty || !active.isEmpty {
                                activityDivider
                            }
                            ActivityPanelSectionTitle("Recent")
                            ForEach(recent) { session in
                                ActivityPanelRow(
                                    session: session,
                                    working: false,
                                    blocked: false,
                                    projectName: projectsByID[session.projectID]?.name
                                ) {
                                    onSelect(session)
                                }
                            }
                        }
                    }
                }
                .padding(EdgeInsets(top: 16, leading: 12, bottom: 20, trailing: 12))
            }
        }
        .environment(\.colorScheme, .dark)
    }

    private var activityDivider: some View {
        Divider()
            .overlay(.white.opacity(0.10))
            .padding(.vertical, 8)
    }
}

struct ActivityPanelSectionTitle: View {
    let title: String

    init(_ title: String) {
        self.title = title
    }

    var body: some View {
        Text(title)
            .font(.system(size: 11, weight: .semibold))
            .foregroundStyle(.white.opacity(0.46))
            .textCase(.uppercase)
            .padding(EdgeInsets(top: 3, leading: 8, bottom: 6, trailing: 8))
    }
}

struct ActivityPanelRow: View {
    let session: RemoteSessionSummary
    let working: Bool
    let blocked: Bool
    var projectName: String? = nil
    let onSelect: () -> Void

    var body: some View {
        Button(action: onSelect) {
            HStack(spacing: 10) {
                Group {
                    if working {
                        TitlebarBrailleSpinner(color: IOSSidebarTheme.toolSpinnerColor(for: session))
                    } else if blocked {
                        Circle()
                            .fill(IOSSidebarTheme.attention)
                            .frame(width: 7, height: 7)
                            .background(
                                Circle()
                                    .fill(IOSSidebarTheme.attention.opacity(0.20))
                                    .frame(width: 15, height: 15)
                            )
                    } else if session.unread {
                        Circle()
                            .fill(IOSSidebarTheme.unread)
                            .frame(width: 7, height: 7)
                    } else {
                        Color.clear
                    }
                }
                .frame(width: 22, height: 22)

                VStack(alignment: .leading, spacing: 2) {
                    Text(session.title)
                        .font(.system(size: 14, weight: .medium))
                        .foregroundStyle(.white.opacity(0.90))
                        .lineLimit(1)
                    Text(subtitle)
                        .font(.system(size: 11, weight: .regular))
                        .foregroundStyle(.white.opacity(0.48))
                        .lineLimit(1)
                }
                Spacer(minLength: 6)
                HStack(spacing: 8) {
                    // TimelineView so the age self-refreshes: the row's
                    // content-equality gating would otherwise freeze a
                    // render-time "3m" label indefinitely.
                    TimelineView(.periodic(from: .now, by: 60)) { _ in
                        Text(trailingStatus)
                            .font(.system(size: 10, weight: .medium))
                            .foregroundStyle(
                                blocked
                                    ? IOSSidebarTheme.attention
                                    : session.latestAlertBody == nil
                                        ? .white.opacity(0.42) : IOSSidebarTheme.unread
                            )
                    }

                    SharedToolIconView(
                        providerID: session.presentationProviderID,
                        command: session.command,
                        size: 16
                    )
                    .opacity(0.74)
                }
            }
            .padding(EdgeInsets(top: 7, leading: 8, bottom: 7, trailing: 8))
        }
        .buttonStyle(.plain)
    }

    private var subtitle: String {
        if let alert = session.latestAlertBody, !alert.isEmpty {
            return "Alert · \(alert)"
        }
        let context = projectName ?? session.presentationProviderID ?? "Terminal"
        return [context, session.worktreeBranch].compactMap { $0 }.joined(separator: "  ")
    }

    private var trailingStatus: String {
        if blocked { return "Blocked" }
        if working { return "run" }
        let age = RelativeAge.shortString(
            fromUnixMs: session.updatedAtUnixMs ?? session.createdAtUnixMs
        )
        return session.latestAlertBody == nil ? age : "Alert · \(age)"
    }

}

struct TitlebarBrailleSpinner: View {
    let color: Color

    var body: some View {
        TimelineView(.periodic(from: .now, by: 0.12)) { context in
            Text(frame(for: context.date))
                .font(.system(size: 14.7, weight: .bold, design: .monospaced))
                .foregroundStyle(color)
                .shadow(color: color.opacity(0.45), radius: 3)
                .frame(width: 16, height: 16)
        }
        .accessibilityHidden(true)
    }

    private func frame(for date: Date) -> String {
        let frames = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"]
        let index = Int(date.timeIntervalSinceReferenceDate / 0.12)
        return frames[index % frames.count]
    }
}

struct EmptyTerminalState: View {
    var store: RemotePreviewStore
    @EnvironmentObject private var connection: RemoteConnectionStore

    var body: some View {
        VStack(spacing: 16) {
            // The user parks on this screen while the app connects (or fails
            // to): the website's animated pixel mascot keeps it company
            // instead of a static brand mark + wifi-slash glyph.
            PixelMascotView(size: 76)
                .padding(.bottom, 4)
            if disconnected {
                if unreachable {
                    Text(connection.pairedMacName.map { "Can't reach \($0)" }
                        ?? "Not connected to a Mac")
                        .font(.subheadline)
                        .foregroundStyle(.white.opacity(0.6))
                    Button {
                        connection.pairingSheetPresented = true
                    } label: {
                        Label(
                            connection.pairedMacName == nil ? "Pair with your Mac" : "Connection…",
                            systemImage: "personalhotspot"
                        )
                    }
                    .liquidGlassProminentButton(tint: .cyan)
                } else {
                    // A young outage is ordinary connecting — cold launch,
                    // foreground resume, the Mac restarting — so keep it
                    // calm: no failure language and no Connection… escape
                    // hatch until the store's grace period lapses.
                    Text(connection.pairedMacName.map { "Connecting to \($0)…" }
                        ?? "Connecting…")
                        .font(.subheadline)
                        .foregroundStyle(.white.opacity(0.6))
                }
            } else {
                Button {
                    store.showSessions()
                } label: {
                    Label("Open Sessions", systemImage: "sidebar.left")
                }
                .liquidGlassProminentButton()
            }
        }
    }

    private var disconnected: Bool {
        store.lastError != nil && store.snapshot.sessions.isEmpty
    }

    /// Failure phase: the store's grace period lapsed, or there is no paired
    /// Mac at all (nothing is being retried, so "connecting" would be a lie).
    private var unreachable: Bool {
        store.isUnreachable || connection.pairedMacName == nil
    }
}
