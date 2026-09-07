import SwiftUI

/// Native interpretation of Page's closed, read-only Content body slot.
/// The Rust App owns keyed styled lines; this view owns scrolling and exposes
/// platform text selection without becoming an editor.
@MainActor
struct ReadOnlyContentBody: View {
    let content: UIContentSpec
    let onAction: (UIAction) -> Void
    @State private var selected: UIContentSelection?

    init(content: UIContentSpec, onAction: @escaping (UIAction) -> Void) {
        self.content = content
        self.onAction = onAction
        _selected = State(initialValue: content.selection)
    }

    var body: some View {
        if content.lines.isEmpty {
            ContentUnavailableView(
                content.emptyMessage,
                systemImage: "doc.text"
            )
            .frame(maxWidth: .infinity, maxHeight: .infinity)
        } else {
            ScrollView(content.wrap ? .vertical : [.horizontal, .vertical]) {
                LazyVStack(alignment: .leading, spacing: 0) {
                    ForEach(content.lines) { line in
                        lineView(line)
                    }
                }
                .frame(maxWidth: content.wrap ? .infinity : nil, alignment: .leading)
                .padding(.horizontal)
                .padding(.vertical, 10)
                .textSelection(.enabled)
            }
            .accessibilityLabel(content.label)
            .onChange(of: content.selection) { _, value in selected = value }
        }
    }

    private func lineView(_ line: UIContentLine) -> some View {
        composedText(line)
            .font(content.font == .monospace ? .system(.body, design: .monospaced) : .body)
            .frame(maxWidth: content.wrap ? .infinity : nil, alignment: .leading)
            .fixedSize(horizontal: !content.wrap, vertical: true)
            .padding(.vertical, content.font == .monospace ? 1 : 2)
            .padding(.horizontal, 4)
            .background(lineBackground(line), in: RoundedRectangle(cornerRadius: 3))
            .contentShape(Rectangle())
            .onTapGesture { select(line.id) }
            .contextMenu {
                if let menu = content.contextMenu {
                    menuButtons(menu, targetID: line.id)
                }
            }
    }

    private func select(_ lineID: String) {
        guard let action = content.select else { return }
        let selection = UIContentSelection(anchorID: lineID, headID: lineID)
        guard selected != selection else { return }
        selected = selection
        onAction(UIAction(
            nodeID: content.id,
            action: action,
            kind: .select,
            value: .textList([lineID, lineID])
        ))
    }

    @ViewBuilder
    private func menuButtons(_ menu: UIMenuSpec, targetID: String) -> some View {
        ForEach(menu.items) { item in
            Button(item.label, role: item.role == .danger ? .destructive : nil) {
                selected = UIContentSelection(anchorID: targetID, headID: targetID)
                onAction(UIAction(
                    nodeID: item.id,
                    action: item.action,
                    kind: .activate,
                    value: .text(targetID)
                ))
            }
            .disabled(item.disabled)
        }
    }

    private func composedText(_ line: UIContentLine) -> Text {
        line.runs.reduce(Text("")) { result, run in
            var fragment = Text(run.text).foregroundStyle(color(run.tone))
            switch run.emphasis {
            case .regular: break
            case .strong: fragment = fragment.bold()
            case .italic: fragment = fragment.italic()
            }
            return result + fragment
        }
        .fontWeight(line.tone == .header ? .semibold : nil)
    }

    private func lineBackground(_ line: UIContentLine) -> Color {
        if isSelected(line.id) { return Color.accentColor.opacity(0.23) }
        switch line.tone {
        case .added: return Color.green.opacity(0.14)
        case .removed: return Color.red.opacity(0.14)
        case .header: return Color.accentColor.opacity(0.08)
        case .default, .muted: return .clear
        }
    }

    private func isSelected(_ id: String) -> Bool {
        guard let selected,
              let anchor = content.lines.firstIndex(where: { $0.id == selected.anchorID }),
              let head = content.lines.firstIndex(where: { $0.id == selected.headID }),
              let index = content.lines.firstIndex(where: { $0.id == id })
        else { return false }
        return (min(anchor, head)...max(anchor, head)).contains(index)
    }

    private func color(_ tone: UIContentTone) -> Color {
        switch tone {
        case .default: .primary
        case .muted: .secondary
        case .accent: .accentColor
        case .info: .blue
        case .success: .green
        case .warning: .orange
        case .danger: .red
        }
    }
}
