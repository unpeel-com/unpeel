//
//  MCPApprovalPanel.swift
//  UnpeelNative
//
//  Desktop surfaces for pending MCP approvals and the computer-permissions
//  nudge. MCP grants render as an in-pane overlay on the Session they belong
//  to (never a floating window, never NSAlert.runModal() — a nested modal
//  run loop inside a main-actor job stalls mobile bootstrap). The TCC nudge
//  still uses `FloatingPromptPanelController` because those grants live in
//  System Settings, not in a Session.
//

import AppKit
import SwiftUI

/// A reusable floating alert-style panel: titled (so it can become key for
/// keyboard shortcuts) but with the title bar hidden, floating level so it
/// stays visible over the app, never modal.
@MainActor
final class FloatingPromptPanelController {
    private var panel: NSPanel?

    /// Show (or update) the panel with the given content. Activates the app
    /// and centers the panel when it was not already visible; a visible
    /// panel keeps its position and just swaps content.
    func show<Content: View>(_ content: Content) {
        let root = AnyView(content)
        if let panel, panel.isVisible {
            (panel.contentViewController as? NSHostingController<AnyView>)?.rootView = root
            return
        }
        let panel = self.panel ?? makePanel()
        self.panel = panel
        panel.contentViewController = NSHostingController(rootView: root)
        // Get the user's eyes on the prompt: the asking agent's tool call is
        // blocked until someone answers (~2 minute timeout).
        NSApp.activate(ignoringOtherApps: true)
        panel.center()
        panel.makeKeyAndOrderFront(nil)
    }

    func dismiss() {
        panel?.orderOut(nil)
        panel?.contentViewController = nil
    }

    private func makePanel() -> NSPanel {
        let panel = NSPanel(
            contentRect: .zero,
            styleMask: [.titled, .fullSizeContentView],
            backing: .buffered,
            defer: true
        )
        panel.titleVisibility = .hidden
        panel.titlebarAppearsTransparent = true
        panel.isMovableByWindowBackground = true
        panel.level = .floating
        panel.hidesOnDeactivate = false
        panel.isReleasedWhenClosed = false
        panel.standardWindowButton(.closeButton)?.isHidden = true
        panel.standardWindowButton(.miniaturizeButton)?.isHidden = true
        panel.standardWindowButton(.zoomButton)?.isHidden = true
        return panel
    }
}

/// In-pane Allow / Don't Allow card for one pending MCP approval. No
/// click-away: this is a privilege grant, so the only exits are the two
/// explicit answers (or a paired controller answering first). Return and
/// Escape are swallowed while this pane is focused so they never reach the
/// terminal underneath.
struct McpApprovalPaneOverlay: View {
    @ObservedObject var store: UnpeelStore
    let approval: PendingMcpApproval
    let capturesKeys: Bool

    var body: some View {
        let message = store.mcpApprovalMessage(approval)
        let moreWaiting = max(
            0,
            store.pendingMcpApprovalCount(forSessionID: approval.presentationSessionID(
                knownIDs: store.mcpApprovalKnownSessionIDs
            )) - 1
        )
        ZStack {
            // Light dim so Liquid Glass still has terminal color to refract;
            // a heavy scrim made the card read as a solid slab.
            Color.black.opacity(0.12)
                .contentShape(Rectangle())

            VStack(alignment: .leading, spacing: 10) {
                HStack(alignment: .top, spacing: 10) {
                    AttentionDot(color: Theme.attention)
                        .padding(.top, 3)
                    Text(message.title)
                        .font(.system(size: 13, weight: .semibold))
                        .foregroundStyle(Theme.foreground)
                        .fixedSize(horizontal: false, vertical: true)
                }
                Text(message.body)
                    .font(.system(size: 12))
                    .foregroundStyle(Theme.mutedForeground)
                    .fixedSize(horizontal: false, vertical: true)
                if moreWaiting > 0 {
                    Text("\(moreWaiting) more waiting")
                        .font(.system(size: 11))
                        .foregroundStyle(Theme.mutedForeground.opacity(0.7))
                }
                HStack(spacing: 8) {
                    Spacer()
                    McpApprovalDialogButton(
                        label: "Don't Allow",
                        prominent: false,
                        action: {
                            store.answerMcpApproval(id: approval.id, approved: false)
                        }
                    )
                    McpApprovalDialogButton(
                        label: "Allow",
                        prominent: true,
                        action: {
                            store.answerMcpApproval(id: approval.id, approved: true)
                        }
                    )
                }
                .padding(.top, 6)
            }
            .padding(20)
            .frame(width: 360)
            .mcpApprovalGlassCard()
        }
        .background(
            McpApprovalKeyMonitor(
                enabled: capturesKeys,
                onAllow: { store.answerMcpApproval(id: approval.id, approved: true) },
                onDeny: { store.answerMcpApproval(id: approval.id, approved: false) }
            )
        )
    }
}

private struct McpApprovalGlassCard: ViewModifier {
    var cornerRadius: CGFloat = 20

    func body(content: Content) -> some View {
        let shape = RoundedRectangle(cornerRadius: cornerRadius, style: .continuous)
        if #available(macOS 26.0, *) {
            content
                .glassEffect(.regular, in: shape)
                .shadow(color: .black.opacity(0.28), radius: 28, y: 10)
        } else {
            content
                .background(
                    ZStack {
                        VisualEffectBackground(
                            material: .popover, blendingMode: .withinWindow
                        )
                        Theme.contentTint
                    }
                    .clipShape(shape)
                    .shadow(color: .black.opacity(0.32), radius: 28, y: 10)
                )
                .overlay(shape.strokeBorder(Theme.foreground.opacity(0.10)))
        }
    }
}

private extension View {
    func mcpApprovalGlassCard(cornerRadius: CGFloat = 20) -> some View {
        modifier(McpApprovalGlassCard(cornerRadius: cornerRadius))
    }
}

private struct McpApprovalDialogButton: View {
    let label: String
    let prominent: Bool
    let action: () -> Void

    var body: some View {
        styled(
            Button(label, action: action)
                .controlSize(.regular)
        )
    }

    @ViewBuilder
    private func styled(_ button: some View) -> some View {
        if #available(macOS 26.0, *) {
            if prominent {
                button.buttonStyle(.glassProminent).tint(Theme.ctaTint)
            } else {
                // `.glass` reads brighter than glassProminent on dark
                // vibrancy and inverts the hierarchy — same choice as
                // EditorButton in Settings.
                button.buttonStyle(.bordered)
            }
        } else {
            if prominent {
                button.buttonStyle(.borderedProminent).tint(Theme.ctaTint)
            } else {
                button.buttonStyle(.bordered)
            }
        }
    }
}

/// Return allows, Escape denies. Installed only on the focused pane so two
/// visible overlays cannot both consume the same key.
private struct McpApprovalKeyMonitor: NSViewRepresentable {
    let enabled: Bool
    let onAllow: () -> Void
    let onDeny: () -> Void

    func makeNSView(context: Context) -> MonitorView {
        let view = MonitorView()
        view.onAllow = onAllow
        view.onDeny = onDeny
        view.enabled = enabled
        return view
    }

    func updateNSView(_ view: MonitorView, context: Context) {
        view.onAllow = onAllow
        view.onDeny = onDeny
        view.enabled = enabled
        view.syncMonitors()
    }

    static func dismantleNSView(_ view: MonitorView, coordinator _: ()) {
        view.removeMonitor()
    }

    final class MonitorView: NSView {
        var onAllow: (() -> Void)?
        var onDeny: (() -> Void)?
        var enabled = false
        private var monitor: Any?

        override func hitTest(_: NSPoint) -> NSView? { nil }

        override func viewDidMoveToWindow() {
            super.viewDidMoveToWindow()
            syncMonitors()
        }

        func syncMonitors() {
            removeMonitor()
            guard enabled, window != nil else { return }
            monitor = NSEvent.addLocalMonitorForEvents(matching: .keyDown) { [weak self] event in
                guard let self, self.enabled else { return event }
                let modifiers = event.modifierFlags.intersection(.deviceIndependentFlagsMask)
                if modifiers.contains(.command) || modifiers.contains(.option)
                    || modifiers.contains(.control)
                {
                    return event
                }
                switch event.keyCode {
                case 36, 76:
                    self.onAllow?()
                    return nil
                case 53:
                    self.onDeny?()
                    return nil
                default:
                    return event
                }
            }
        }

        func removeMonitor() {
            if let monitor {
                NSEvent.removeMonitor(monitor)
                self.monitor = nil
            }
        }
    }
}

/// Content for the computer-permissions nudge (missing TCC grants after a
/// computer-use approval or a failing action) — one Grant button per missing
/// permission, plus Not Now.
struct ComputerPermissionsNudgeView: View {
    let missing: [String]
    let subject: String
    let onGrant: (String) -> Void
    let onDismiss: () -> Void

    var body: some View {
        VStack(spacing: 12) {
            Image(nsImage: NSApp.applicationIconImage)
                .resizable()
                .frame(width: 48, height: 48)
            Text("Computer use needs macOS permissions")
                .font(.system(size: 13, weight: .semibold))
                .multilineTextAlignment(.center)
                .fixedSize(horizontal: false, vertical: true)
            Text("\(subject) tried to control this Mac, but Unpeel is missing: "
                + missing.joined(separator: ", ")
                + ". Grant them to Unpeel in System Settings ▸ Privacy & Security — "
                + "Settings ▸ Computer shows live status.")
                .font(.system(size: 11))
                .foregroundStyle(.secondary)
                .multilineTextAlignment(.center)
                .fixedSize(horizontal: false, vertical: true)
            VStack(spacing: 6) {
                ForEach(missing, id: \.self) { permission in
                    Button("Grant \(permission)") { onGrant(permission) }
                }
                Button("Not Now") { onDismiss() }
                    .keyboardShortcut(.cancelAction)
            }
            .padding(.top, 4)
        }
        .padding(20)
        .frame(width: 380)
    }
}
