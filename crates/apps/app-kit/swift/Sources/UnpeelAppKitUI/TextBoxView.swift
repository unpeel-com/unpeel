import SwiftUI

/// Native SwiftUI interpretation of the closed `textBox` root component.
///
/// The plain configuration is a rounded bordered multi-line field. A prompt
/// glyph, border titles, a busy status row, and key hints turn it into a
/// chat-style prompt bar. Local edits stay in a draft until the renderer
/// emits `set-text` (`change`) or `submit`; a fresh server text replaces
/// the draft.
public struct TextBoxView: View {
    public let snapshot: UISnapshot
    public let onAction: (UIAction) -> Void

    public init(snapshot: UISnapshot, onAction: @escaping (UIAction) -> Void) {
        self.snapshot = snapshot
        self.onAction = onAction
    }

    public var body: some View {
        switch snapshot.root.component {
        case let .textBox(textBox):
            TextBoxContent(nodeID: snapshot.root.id, spec: textBox, onAction: onAction)
        case .canvasPage, .markdownEditor, .media, .menu, .page, .surface, .tree, .unsupported:
            EmptyView()
        }
    }
}

private struct TextBoxContent: View {
    let nodeID: String
    let spec: TextBoxSpec
    let onAction: (UIAction) -> Void

    @State private var draft: String
    @State private var serverText: String
    @FocusState private var focused: Bool

    init(nodeID: String, spec: TextBoxSpec, onAction: @escaping (UIAction) -> Void) {
        self.nodeID = nodeID
        self.spec = spec
        self.onAction = onAction
        _draft = State(initialValue: spec.text)
        _serverText = State(initialValue: spec.text)
    }

    private var lineHeight: CGFloat { 18 }

    var body: some View {
        VStack(alignment: .leading, spacing: 4) {
            if let busy = spec.busy {
                statusRow(busy)
            }
            editorBox
            if !spec.hints.isEmpty {
                hintRow
            }
        }
        .onChange(of: spec.text) { _, next in
            // Adopt authoritative text only when the server actually changed it.
            if next != serverText {
                serverText = next
                draft = next
            }
        }
    }

    private func statusRow(_ busy: TextBoxBusy) -> some View {
        HStack(spacing: 6) {
            ProgressView()
                .controlSize(.small)
            Text(busy.label)
            Text(String(format: "%.1fs", Double(busy.elapsedMs) / 1000))
                .foregroundStyle(.secondary)
            Spacer()
            if !busy.rightMeta.isEmpty {
                Text(busy.rightMeta)
                    .foregroundStyle(.secondary)
            }
        }
        .font(.system(.body, design: .monospaced))
        .padding(.horizontal, 4)
        .accessibilityElement(children: .combine)
    }

    private var editorBox: some View {
        HStack(alignment: .top, spacing: 4) {
            if !spec.prompt.isEmpty {
                Text(spec.prompt)
                    .foregroundStyle(.secondary)
                    .padding(.top, 8)
            }
            ZStack(alignment: .topLeading) {
                if draft.isEmpty, !spec.placeholder.isEmpty {
                    Text(spec.placeholder)
                        .foregroundStyle(.tertiary)
                        .padding(.top, 8)
                        .padding(.leading, 5)
                        .allowsHitTesting(false)
                }
                TextEditor(text: $draft)
                    .scrollContentBackground(.hidden)
                    .focused($focused)
                    .frame(
                        minHeight: CGFloat(spec.minRows) * lineHeight + 16,
                        maxHeight: CGFloat(spec.maxRows) * lineHeight + 16
                    )
                    .onKeyPress(.return, phases: .down, action: handleReturn)
                    .onChange(of: draft) { _, next in
                        guard next != serverText, let action = spec.actions.setText else { return }
                        onAction(UIAction(
                            nodeID: nodeID,
                            action: action,
                            kind: .change,
                            value: .text(next)
                        ))
                    }
            }
        }
        .font(.system(.body, design: .monospaced))
        .padding(.horizontal, 8)
        .overlay {
            RoundedRectangle(cornerRadius: 8, style: .continuous)
                .stroke(focused ? Color.secondary : Color.secondary.opacity(0.4))
        }
        .overlay(alignment: .topLeading) { title(at: .topLeft) }
        .overlay(alignment: .topTrailing) { title(at: .topRight) }
        .overlay(alignment: .bottomLeading) { title(at: .bottomLeft) }
        .overlay(alignment: .bottomTrailing) { title(at: .bottomRight) }
        .accessibilityLabel(spec.placeholder)
    }

    @ViewBuilder
    private func title(at position: TextBoxTitlePosition) -> some View {
        if let title = spec.titles.first(where: { $0.position == position }) {
            Text(title.text)
                .font(.caption)
                .foregroundStyle(.secondary)
                .padding(.horizontal, 4)
                .background(.background)
                .padding(.horizontal, 10)
                .offset(y: position == .topLeft || position == .topRight ? -8 : 8)
                .allowsHitTesting(false)
        }
    }

    private var hintRow: some View {
        HStack(spacing: 0) {
            ForEach(Array(spec.hints.enumerated()), id: \.offset) { index, hint in
                if index > 0 {
                    Text(" │ ").foregroundStyle(.tertiary)
                }
                Text(hint.key).bold()
                Text(":" + hint.label).foregroundStyle(.secondary)
            }
        }
        .font(.system(.caption, design: .monospaced))
        .padding(.horizontal, 4)
    }

    private func handleReturn(_ press: KeyPress) -> KeyPress.Result {
        guard spec.submitMode == .enter,
              press.modifiers.isDisjoint(with: [.shift, .option, .command, .control])
        else { return .ignored }
        submit()
        return .handled
    }

    private func submit() {
        guard let action = spec.actions.submit,
              !draft.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty
        else { return }
        onAction(UIAction(nodeID: nodeID, action: action, kind: .submit, value: .text(draft)))
        draft = ""
    }
}
