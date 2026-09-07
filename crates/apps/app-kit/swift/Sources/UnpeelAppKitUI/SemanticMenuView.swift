import SwiftUI

/// Native interpretation of the closed Menu component. The renderer resolves
/// caret/pointer/control anchors locally and returns only declared actions.
@MainActor
public struct SemanticMenuView: View {
    public let snapshot: UISnapshot
    public let onAction: (UIAction) -> Void

    public init(snapshot: UISnapshot, onAction: @escaping (UIAction) -> Void) {
        self.snapshot = snapshot
        self.onAction = onAction
    }

    public var body: some View {
        switch snapshot.root.component {
        case let .menu(menu):
            SemanticMenuContent(ownerID: snapshot.root.id, menu: menu, onAction: onAction)
                .padding(8)
        case .canvasPage, .markdownEditor, .media, .page, .surface, .textBox, .tree, .unsupported:
            EmptyView()
        }
    }
}

@MainActor
struct SemanticMenuContent: View {
    let ownerID: String
    let menu: UIMenuSpec
    let onAction: (UIAction) -> Void
    @State private var selectedID: String?

    init(
        ownerID: String,
        menu: UIMenuSpec,
        onAction: @escaping (UIAction) -> Void
    ) {
        self.ownerID = ownerID
        self.menu = menu
        self.onAction = onAction
        _selectedID = State(initialValue: menu.selectedID ?? menu.items.first(where: { !$0.disabled })?.id)
    }

    var body: some View {
        VStack(alignment: .leading, spacing: 2) {
            ForEach(menu.items) { item in
                Button {
                    activate(item)
                } label: {
                    HStack(spacing: 10) {
                        if let hint = item.hint {
                            Text(hint)
                                .font(.system(.body, design: .monospaced))
                                .foregroundStyle(.secondary)
                                .frame(minWidth: 42, alignment: .leading)
                        }
                        Text(item.label)
                        Spacer(minLength: 16)
                    }
                    .contentShape(Rectangle())
                    .padding(.horizontal, 9)
                    .frame(minHeight: 28)
                    .background(
                        selectedID == item.id ? Color.accentColor.opacity(0.18) : .clear,
                        in: RoundedRectangle(cornerRadius: 5)
                    )
                }
                .buttonStyle(.plain)
                .foregroundStyle(item.role == .danger ? Color.red : Color.primary)
                .disabled(item.disabled)
                .opacity(item.disabled ? 0.45 : 1)
                .onHover { hovering in
                    if hovering, !item.disabled { selectedID = item.id }
                }
            }
        }
        .accessibilityElement(children: .contain)
        .accessibilityLabel(menu.label)
        .focusable()
        .onKeyPress(phases: [.down, .repeat]) { press in
            handleKey(press)
        }
        .onChange(of: menu.selectedID) { _, selected in
            selectedID = selected ?? menu.items.first(where: { !$0.disabled })?.id
        }
    }

    private var enabledItems: [UIMenuItemSpec] {
        menu.items.filter { !$0.disabled }
    }

    private func activate(_ item: UIMenuItemSpec) {
        guard !item.disabled else { return }
        onAction(UIAction(nodeID: item.id, action: item.action, kind: .activate))
    }

    private func handleKey(_ press: KeyPress) -> KeyPress.Result {
        guard press.modifiers.intersection([.command, .control, .option]).isEmpty else {
            return .ignored
        }
        let items = enabledItems
        let current = max(0, items.firstIndex(where: { $0.id == selectedID }) ?? 0)
        let target: Int?
        switch press.key {
        case .upArrow: target = (current - 1 + items.count) % max(1, items.count)
        case .downArrow: target = (current + 1) % max(1, items.count)
        case .home: target = 0
        case .end: target = items.count - 1
        case .return, .space:
            guard let item = items[safe: current] else { return .ignored }
            activate(item)
            return .handled
        case .escape:
            guard let dismiss = menu.dismiss else { return .ignored }
            onAction(UIAction(nodeID: ownerID, action: dismiss, kind: .cancel))
            return .handled
        default: return .ignored
        }
        guard let target, let item = items[safe: target] else { return .ignored }
        selectedID = item.id
        return .handled
    }
}

private extension Collection {
    subscript(safe index: Index) -> Element? {
        indices.contains(index) ? self[index] : nil
    }
}
