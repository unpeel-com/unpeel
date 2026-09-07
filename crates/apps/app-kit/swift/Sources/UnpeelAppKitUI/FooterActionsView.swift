import SwiftUI

/// Native interpretation of Rust-owned screen footer actions.
@MainActor
public struct FooterActionsView: View {
    public let footer: UIFooterActionsSpec
    public let onAction: (UIAction) -> Void

    public init(footer: UIFooterActionsSpec, onAction: @escaping (UIAction) -> Void) {
        self.footer = footer
        self.onAction = onAction
    }

    public var body: some View {
        if !footer.actions.isEmpty {
            VStack(spacing: 0) {
                Divider()
                HStack(spacing: 8) {
                    ForEach(footer.actions) { action in
                        footerButton(action)
                    }
                    Spacer(minLength: 0)
                }
                .padding(.horizontal, 10)
                .frame(minHeight: 38)
                .background(.bar)
            }
        }
    }

    @ViewBuilder
    private func footerButton(_ action: UIFooterActionSpec) -> some View {
        let button = Button(
            role: action.role == .danger ? .destructive : nil,
            action: {
                onAction(UIAction(
                    nodeID: action.id,
                    action: action.action,
                    kind: .activate
                ))
            },
            label: {
                HStack(spacing: 5) {
                    if let accelerator = action.accelerator {
                        Text(displayLabel(accelerator))
                            .font(.system(.caption, design: .monospaced).weight(.semibold))
                            .foregroundStyle(.secondary)
                    }
                    if action.busy {
                        ProgressView()
                            .controlSize(.mini)
                            .accessibilityLabel("In progress")
                    }
                    Text(action.label)
                }
            }
        )
        .disabled(action.disabled)

        if let shortcut = keyboardShortcut(action.accelerator) {
            button.keyboardShortcut(shortcut.key, modifiers: shortcut.modifiers)
        } else {
            button
        }
    }

    private func displayLabel(_ accelerator: String) -> String {
        if let key = accelerator.dropPrefix("ctrl+") {
            return "⌃\(key.uppercased())"
        }
        switch accelerator {
        case "escape": return "Esc"
        case "enter": return "↩"
        case "space": return "Space"
        default: return accelerator
        }
    }

    private func keyboardShortcut(_ accelerator: String?) -> (
        key: KeyEquivalent,
        modifiers: EventModifiers
    )? {
        guard let accelerator else { return nil }
        if let key = accelerator.dropPrefix("ctrl+"), let character = key.first {
            return (KeyEquivalent(character), .control)
        }
        switch accelerator {
        case "escape": return (.escape, [])
        case "enter": return (.return, [])
        case "space": return (KeyEquivalent(" "), [])
        default:
            guard let character = accelerator.first else { return nil }
            return (KeyEquivalent(character), [])
        }
    }
}

private extension String {
    func dropPrefix(_ prefix: String) -> String? {
        hasPrefix(prefix) ? String(dropFirst(prefix.count)) : nil
    }
}
