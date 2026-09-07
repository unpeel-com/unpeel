import SwiftUI
import UnpeelAppKitUI

struct ContentView: View {
    @ObservedObject var host: MiniHost

    var body: some View {
        NavigationSplitView {
            sidebar
                .navigationSplitViewColumnWidth(min: 220, ideal: 250, max: 310)
        } detail: {
            if let session = host.selectedSession {
                SessionDetail(session: session)
                    .id(session.id)
            } else if let error = host.buildError {
                ContentUnavailableView(
                    "Could not prepare examples",
                    systemImage: "exclamationmark.triangle",
                    description: Text(error)
                )
            } else {
                VStack(spacing: 14) {
                    ProgressView()
                    Text(host.buildMessage)
                        .foregroundStyle(.secondary)
                }
            }
        }
        .navigationTitle("App Kit Kitchen Sink")
    }

    private var sidebar: some View {
        List(selection: $host.selectedSessionID) {
            Section("Live app sessions") {
                ForEach(host.sessions) { session in
                    SessionRow(session: session)
                        .tag(session.id)
                }
            }
            Section("Mini-host") {
                Label("No Unpeel required", systemImage: "shippingbox")
                Label("libghostty PTYs + Unix sockets", systemImage: "terminal")
                Label("Scoped participant tokens", systemImage: "key")
            }
            .foregroundStyle(.secondary)
        }
        .safeAreaInset(edge: .bottom) {
            HStack(spacing: 8) {
                Circle()
                    .fill(host.buildError == nil ? Color.green : Color.red)
                    .frame(width: 7, height: 7)
                Text(host.buildMessage)
                    .font(.caption)
                    .lineLimit(1)
                Spacer()
            }
            .padding(10)
            .background(.bar)
        }
    }
}

private struct SessionRow: View {
    @ObservedObject var session: HostedAppSession

    var body: some View {
        HStack(spacing: 10) {
            Image(systemName: session.kind.systemImage)
                .frame(width: 20)
                .foregroundStyle(.tint)
            VStack(alignment: .leading, spacing: 3) {
                Text(session.title)
                    .fontWeight(.medium)
                Text("\(session.processState.label) · \(session.connectionLabel)")
                    .font(.caption2)
                    .foregroundStyle(.secondary)
                    .lineLimit(1)
            }
            Spacer(minLength: 0)
            Circle()
                .fill(session.processState.isRunning ? Color.green : Color.orange)
                .frame(width: 7, height: 7)
        }
        .padding(.vertical, 3)
    }
}

private struct SessionDetail: View {
    @ObservedObject var session: HostedAppSession
    @State private var showsComponentTree = true

    var body: some View {
        VStack(spacing: 0) {
            sessionToolbar
            Divider()
            HSplitView {
                presentation
                    .frame(maxWidth: .infinity, maxHeight: .infinity)
                    .padding(10)
                if showsComponentTree {
                    ComponentTreeCard(session: session)
                        .frame(minWidth: 270, idealWidth: 330, maxWidth: 500)
                }
            }
            Divider()
            HarnessPanel(session: session)
        }
        .background(Color(nsColor: .windowBackgroundColor))
    }

    private var sessionToolbar: some View {
        HStack(spacing: 12) {
            Label(session.title, systemImage: session.kind.systemImage)
                .font(.headline)
            StatusPill(
                text: session.processState.label,
                color: session.processState.isRunning ? .green : .orange
            )
            StatusPill(
                text: session.connectionLabel,
                color: session.connectionState.isAttached ? .teal : .secondary
            )
            StatusPill(
                text: session.deliveryLabel,
                color: session.lastDelivery.isDelta ? .purple : .blue
            )
            if session.kind.isCrossPlatformAuditApp {
                StatusPill(
                    text: session.walkthroughStatus,
                    color: session.walkthroughComplete ? .green : .secondary
                )
            }
            Spacer()
            Picker("Presentation", selection: $session.paneMode) {
                ForEach(PaneMode.allCases) { mode in
                    Text(mode.rawValue.capitalized).tag(mode)
                }
            }
            .labelsHidden()
            .pickerStyle(.segmented)
            .frame(width: 320)
            Button {
                showsComponentTree.toggle()
            } label: {
                Label(
                    "Component Tree",
                    systemImage: showsComponentTree
                        ? "sidebar.trailing"
                        : "sidebar.trailing"
                )
            }
            .help(showsComponentTree ? "Hide component tree" : "Show component tree")
        }
        .padding(.horizontal, 14)
        .frame(height: 50)
    }

    @ViewBuilder
    private var presentation: some View {
        switch session.paneMode {
        case .terminal:
            TerminalCard(session: session)
        case .native:
            ComponentCard(session: session)
        case .web:
            WebComponentCard(session: session)
        case .split:
            HSplitView {
                TerminalCard(session: session)
                    .frame(minWidth: 320)
                ComponentCard(session: session)
                    .frame(minWidth: 340)
                WebComponentCard(session: session)
                    .frame(minWidth: 340)
            }
        }
    }
}

private struct TerminalCard: View {
    @ObservedObject var session: HostedAppSession

    var body: some View {
        ZStack(alignment: .topLeading) {
            Color(red: 0.055, green: 0.063, blue: 0.078)
            if let broker = session.surfaceBroker {
                KitchenSinkSurfacePresenter(
                    broker: broker,
                    interactive: false,
                    background: surfaceBackground
                )
                    .aspectRatio(
                        CGFloat(SurfaceMiniBroker.logicalWidth)
                            / CGFloat(SurfaceMiniBroker.logicalHeight),
                        contentMode: .fit
                    )
                    .frame(maxWidth: .infinity, maxHeight: .infinity)
                    .padding(.top, 28)
                    .allowsHitTesting(false)
            }
            TerminalPane(
                engine: session.terminalEngine,
                autoFocus: session.paneMode == .terminal
            )
                .padding(.top, 28)
            paneHeader("TERMINAL", detail: "libghostty · Metal · real PTY")
        }
        .clipShape(RoundedRectangle(cornerRadius: 9, style: .continuous))
        .overlay {
            RoundedRectangle(cornerRadius: 9, style: .continuous)
                .stroke(Color.white.opacity(0.1))
        }
    }

    private var surfaceBackground: SurfaceBackground {
        guard let snapshot = session.snapshot else { return .transparent }
        switch snapshot.root.component {
        case let .surface(surface):
            return surface.background
        case let .canvasPage(page):
            return page.surface.surface.background
        case .markdownEditor, .media, .menu, .page, .textBox, .tree, .unsupported:
            return .transparent
        }
    }
}

private struct WebComponentCard: View {
    @ObservedObject var session: HostedAppSession

    var body: some View {
        ZStack(alignment: .topLeading) {
            Color(nsColor: .controlBackgroundColor)
            Group {
                if let snapshot = session.snapshot {
                    WebComponentPane(
                        snapshot: snapshot,
                        surfaceEndpoint: session.surfaceBroker?.webEndpoint
                    ) { action in
                        session.sendPrimary(action)
                    }
                } else {
                    ContentUnavailableView(
                        "Waiting for semantic UI",
                        systemImage: "globe",
                        description: Text(session.connectionLabel)
                    )
                }
            }
            .padding(.top, 28)
            paneHeader("WEB", detail: "WKWebView · App Kit DOM components")
        }
        .clipShape(RoundedRectangle(cornerRadius: 9, style: .continuous))
        .overlay {
            RoundedRectangle(cornerRadius: 9, style: .continuous)
                .stroke(Color.primary.opacity(0.1))
        }
    }
}

private struct ComponentCard: View {
    @ObservedObject var session: HostedAppSession

    var body: some View {
        ZStack(alignment: .topLeading) {
            Color(nsColor: .controlBackgroundColor)
            Group {
                if let snapshot = session.snapshot {
                    NativeComponent(
                        snapshot: snapshot,
                        surfaceBroker: session.surfaceBroker
                    ) { action in
                        session.sendPrimary(action)
                    }
                } else {
                    ContentUnavailableView(
                        "Waiting for semantic UI",
                        systemImage: "rectangle.connected.to.line.below",
                        description: Text(session.connectionLabel)
                    )
                }
            }
            .padding(.top, 28)
            paneHeader("NATIVE", detail: "UnpeelAppKitUI · SwiftUI")
            if !session.rendererEnabled {
                VStack {
                    Spacer()
                    Text("Renderer disconnected — cached projection retained for resume")
                        .font(.caption)
                        .padding(8)
                        .background(.ultraThinMaterial, in: Capsule())
                        .padding()
                }
                .frame(maxWidth: .infinity, maxHeight: .infinity)
                .allowsHitTesting(false)
            }
        }
        .clipShape(RoundedRectangle(cornerRadius: 9, style: .continuous))
        .overlay {
            RoundedRectangle(cornerRadius: 9, style: .continuous)
                .stroke(Color.primary.opacity(0.1))
        }
    }
}

private struct NativeComponent: View {
    let snapshot: UISnapshot
    let surfaceBroker: SurfaceMiniBroker?
    let onAction: (UIAction) -> Void

    var body: some View {
        switch snapshot.root.component {
        case let .canvasPage(page):
            if let surfaceBroker {
                CanvasPageView(snapshot: snapshot, onAction: onAction) { surface in
                    KitchenSinkSurfacePresenter(
                        broker: surfaceBroker,
                        interactive: surface.inputPolicy != .none,
                        background: surface.background
                    )
                    .aspectRatio(
                        CGFloat(SurfaceMiniBroker.logicalWidth)
                            / CGFloat(SurfaceMiniBroker.logicalHeight),
                        contentMode: .fit
                    )
                }
            } else {
                ContentUnavailableView(
                    "Canvas presenter unavailable",
                    systemImage: "rectangle.on.rectangle.angled",
                    description: Text(
                        "\(page.surface.surface.reference.streamID) needs UnpeelSurfaceKit; "
                            + "using terminal fallback."
                    )
                )
            }
        case .page:
            PageView(snapshot: snapshot, onAction: onAction)
        case .markdownEditor:
            MarkdownEditorView(snapshot: snapshot, onAction: onAction)
        case .textBox:
            TextBoxView(snapshot: snapshot, onAction: onAction)
                .padding(20)
                .frame(maxWidth: .infinity, maxHeight: .infinity, alignment: .bottom)
        case .media:
            ScrollView([.horizontal, .vertical]) {
                MediaView(snapshot: snapshot, onAction: onAction)
                    .padding(30)
            }
            .frame(maxWidth: .infinity, maxHeight: .infinity)
        case .menu:
            SemanticMenuView(snapshot: snapshot, onAction: onAction)
        case .tree:
            TreeView(snapshot: snapshot, onAction: onAction)
        case let .surface(surface):
            if let surfaceBroker {
                SurfaceComponentView(snapshot: snapshot) { _ in
                    KitchenSinkSurfacePresenter(
                        broker: surfaceBroker,
                        interactive: surface.inputPolicy != .none,
                        background: surface.background
                    )
                    .aspectRatio(
                        CGFloat(SurfaceMiniBroker.logicalWidth)
                            / CGFloat(SurfaceMiniBroker.logicalHeight),
                        contentMode: .fit
                    )
                }
            } else {
                ContentUnavailableView(
                    "Surface presenter unavailable",
                    systemImage: "globe.americas.fill",
                    description: Text(
                        "\(surface.reference.sessionID)/\(surface.reference.streamID) needs "
                            + "UnpeelSurfaceKit and its WebGPU assets; using terminal fallback."
                    )
                )
            }
        case let .unsupported(kind):
            ContentUnavailableView(
                "Unsupported component",
                systemImage: "questionmark.square.dashed",
                description: Text(kind)
            )
        }
    }
}

private struct HarnessPanel: View {
    @ObservedObject var session: HostedAppSession
    private let grants = ["view", "interact", "edit", "command", "admin"]

    var body: some View {
        ScrollView(.horizontal) {
            HStack(alignment: .top, spacing: 22) {
                controlGroup("PROCESS + RENDERER") {
                    HStack {
                        Button("Kill", systemImage: "stop.fill") {
                            session.killProcess()
                        }
                        .disabled(!session.processState.isRunning)
                        Button("Restart", systemImage: "arrow.clockwise") {
                            session.restartProcess()
                        }
                        if session.rendererEnabled {
                            Button("Disconnect UI", systemImage: "bolt.slash") {
                                session.disconnectRenderer()
                            }
                        } else {
                            Button("Reconnect UI", systemImage: "bolt") {
                                session.reconnectRenderer()
                            }
                        }
                    }
                    Text(instanceDescription)
                        .font(.caption)
                        .foregroundStyle(.secondary)
                    if let ack = session.lastAck {
                        Text("Human ack: \(ack.status.rawValue) · r\(ack.revision)")
                            .font(.caption)
                            .foregroundStyle(ack.status == .rejected ? .red : .secondary)
                    }
                }

                Divider().frame(height: 108)

                if session.kind.isCrossPlatformAuditApp {
                    controlGroup("SEMANTIC SCREEN WALK") {
                        HStack {
                            Button("Walk every screen", systemImage: "figure.walk") {
                                session.walkEveryScreen()
                            }
                            Text(session.walkthroughStatus)
                                .font(.caption)
                                .foregroundStyle(
                                    session.walkthroughComplete ? .green : .secondary
                                )
                        }
                        Text(session.observedScreens.isEmpty
                            ? "No screens observed yet"
                            : session.observedScreens.joined(separator: " · "))
                            .font(.caption)
                            .foregroundStyle(.secondary)
                            .lineLimit(2)
                        let missing = session.kind.expectedSemanticScreens.filter {
                            !session.observedScreens.contains($0)
                        }
                        Text("Terminal-only: \(missing.isEmpty ? "none" : missing.joined(separator: ", "))")
                            .font(.caption)
                            .foregroundStyle(missing.isEmpty ? .green : .orange)
                            .lineLimit(1)
                    }

                    Divider().frame(height: 108)
                }

                controlGroup("SECOND PARTICIPANT") {
                    HStack(spacing: 10) {
                        ForEach(grants, id: \.self) { grant in
                            Toggle(grant, isOn: grantBinding(grant))
                                .toggleStyle(.checkbox)
                        }
                    }
                    HStack {
                        Button(
                            session.agentAttached ? "Reattach Agent" : "Attach Agent",
                            systemImage: "person.badge.key"
                        ) {
                            session.attachAgent()
                        }
                        if session.agentAttached {
                            Button("Detach", systemImage: "xmark") {
                                session.detachAgent()
                            }
                            Button("Exercise as Agent", systemImage: "wand.and.stars") {
                                session.exerciseAgent()
                            }
                            .disabled(session.agentSnapshot == nil)
                        }
                    }
                    Text("\(session.agentConnectionLabel) · \(session.agentProjectionLabel)")
                        .font(.caption)
                        .foregroundStyle(.secondary)
                        .lineLimit(1)
                    if let ack = session.agentLastAck {
                        Text("Agent ack: \(ack.status.rawValue) · r\(ack.revision)\(ack.message.map { " · \($0)" } ?? "")")
                            .font(.caption)
                            .foregroundStyle(ack.status == .rejected ? .red : .secondary)
                            .lineLimit(1)
                    }
                }

                Divider().frame(height: 108)

                controlGroup("PRESENCE") {
                    if session.presence.isEmpty {
                        Text("No attached participants")
                            .font(.caption)
                            .foregroundStyle(.secondary)
                    } else {
                        ForEach(session.presence, id: \.clientID) { member in
                            HStack(spacing: 6) {
                                Circle()
                                    .fill(Color(hex: member.participant.color) ?? .secondary)
                                    .frame(width: 7, height: 7)
                                Text(member.participant.displayName ?? member.participant.id)
                                    .font(.caption)
                                Text(member.state.rendererVisible ? "native" : (member.state.terminalVisible ? "terminal" : "hidden"))
                                    .font(.caption2)
                                    .foregroundStyle(.secondary)
                            }
                        }
                    }
                }
            }
            .padding(.horizontal, 14)
            .padding(.vertical, 10)
        }
        .frame(minHeight: 135, idealHeight: 145, maxHeight: 155)
        .background(.bar)
    }

    private var instanceDescription: String {
        let instance = session.appInstanceID.map { String($0.prefix(12)) } ?? "none"
        let resume = session.lastAttachResumed.map { $0 ? "resumed" : "fresh snapshot" } ?? "pending"
        return "appInstanceId \(instance)… · generation \(session.instanceGeneration) · \(resume) · \(session.deliveryLabel)"
    }

    private func grantBinding(_ grant: String) -> Binding<Bool> {
        Binding(
            get: { session.agentGrants.contains(grant) },
            set: { enabled in
                if enabled {
                    session.agentGrants.insert(grant)
                } else {
                    session.agentGrants.remove(grant)
                }
            }
        )
    }

    private func controlGroup<Content: View>(
        _ title: String,
        @ViewBuilder content: () -> Content
    ) -> some View {
        VStack(alignment: .leading, spacing: 8) {
            Text(title)
                .font(.caption2.weight(.semibold))
                .foregroundStyle(.secondary)
            content()
        }
        .frame(minWidth: 285, alignment: .leading)
    }
}

private struct StatusPill: View {
    let text: String
    let color: Color

    var body: some View {
        Text(text)
            .font(.caption2.weight(.medium))
            .foregroundStyle(color)
            .padding(.horizontal, 8)
            .padding(.vertical, 4)
            .background(color.opacity(0.12), in: Capsule())
            .lineLimit(1)
    }
}

private func paneHeader(_ title: String, detail: String) -> some View {
    HStack(spacing: 8) {
        Text(title)
            .font(.system(size: 10, weight: .bold, design: .rounded))
        Text(detail)
            .font(.system(size: 10))
            .foregroundStyle(.secondary)
        Spacer()
    }
    .padding(.horizontal, 10)
    .frame(height: 28)
    .background(.bar)
}

private extension UIUnixSessionClient.ConnectionState {
    var isAttached: Bool {
        if case .attached = self { return true }
        return false
    }
}

private extension Optional where Wrapped == UIProjectionDelivery {
    var isDelta: Bool {
        guard case .delta = self else { return false }
        return true
    }
}

private extension Color {
    init?(hex: String?) {
        guard let hex else { return nil }
        let value = hex.trimmingCharacters(in: CharacterSet(charactersIn: "#"))
        guard value.count == 6, let integer = UInt64(value, radix: 16) else { return nil }
        self.init(
            red: Double((integer >> 16) & 0xff) / 255,
            green: Double((integer >> 8) & 0xff) / 255,
            blue: Double(integer & 0xff) / 255
        )
    }
}
