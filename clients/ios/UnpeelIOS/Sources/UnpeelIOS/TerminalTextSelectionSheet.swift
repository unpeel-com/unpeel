import SwiftUI
#if os(iOS)
import UIKit
#endif

/// What the terminal long-press hands to the selection sheet: the viewport
/// text snapshot and the UTF-16 range of the pressed word within it (nil ⇒
/// select everything). The range was resolved against exactly this text, so
/// the text must be displayed unmodified for the pre-selection to line up.
public struct TerminalTextSelectionPayload: Equatable {
    public let text: String
    public let anchorRange: NSRange?

    public init(text: String, anchorRange: NSRange?) {
        self.text = text
        self.anchorRange = anchorRange
    }
}

#if os(iOS)

/// Root-presented sheet for long-press text selection on the phone terminal.
/// The Metal surface can't host native selection, so the viewport text is
/// re-materialized here as a real `UITextView` — system handles, magnifier,
/// and edit menu included — pre-selected on the word under the finger.
struct TerminalTextSelectionSheet: View {
    let payload: TerminalTextSelectionPayload
    let onDone: () -> Void

    @State private var copiedFeedback = false

    var body: some View {
        VStack(spacing: 0) {
            HStack(spacing: 10) {
                Text("Select text")
                    .font(.system(size: 17, weight: .semibold))
                    .foregroundStyle(IOSSidebarTheme.foreground)

                Spacer(minLength: 0)

                Button {
                    // The raw grid pads every row to the full column count;
                    // "Copy All" is the "give me the screen" action, so strip
                    // the padding noise before it lands in a paste.
                    UIPasteboard.general.string = payload.text
                        .components(separatedBy: "\n")
                        .map { line in
                            String(line.reversed().drop { $0 == " " }.reversed())
                        }
                        .joined(separator: "\n")
                    UIImpactFeedbackGenerator(style: .light).impactOccurred()
                    copiedFeedback = true
                    DispatchQueue.main.asyncAfter(deadline: .now() + 1.2) {
                        copiedFeedback = false
                    }
                } label: {
                    Label(
                        copiedFeedback ? "Copied" : "Copy All",
                        systemImage: copiedFeedback ? "checkmark" : "doc.on.doc"
                    )
                    .font(.system(size: 13, weight: .semibold))
                    .padding(.horizontal, 12)
                    .frame(height: 34)
                }
                .foregroundStyle(IOSSidebarTheme.foreground)
                .iosGlassControl(cornerRadius: 11)

                Button {
                    onDone()
                } label: {
                    Image(systemName: "xmark")
                        .font(.system(size: 13, weight: .semibold))
                        .frame(width: 34, height: 34)
                }
                .foregroundStyle(IOSSidebarTheme.mutedForeground)
                .iosGlassControl(cornerRadius: 11)
                .accessibilityLabel("Close text selection")
            }
            .padding(.horizontal, 18)
            .padding(.top, 18)
            .padding(.bottom, 12)

            Rectangle()
                .fill(.white.opacity(0.08))
                .frame(height: 1)

            SelectableTerminalTextView(
                text: payload.text,
                anchorRange: payload.anchorRange
            )
        }
        .background(TerminalChrome.background)
        .environment(\.colorScheme, .dark)
    }
}

/// Non-editable, selectable `UITextView` showing the viewport snapshot in a
/// terminal-ish monospaced style. Becomes first responder with the anchor
/// word pre-selected so the system selection handles are live immediately —
/// non-editable text views never summon the keyboard.
private struct SelectableTerminalTextView: UIViewRepresentable {
    let text: String
    let anchorRange: NSRange?

    func makeCoordinator() -> Coordinator {
        Coordinator()
    }

    func makeUIView(context: Context) -> UITextView {
        let view = UITextView()
        view.isEditable = false
        view.isSelectable = true
        view.isScrollEnabled = true
        view.alwaysBounceVertical = true
        view.backgroundColor = .clear
        view.font = .monospacedSystemFont(ofSize: 13, weight: .regular)
        view.textColor = UIColor.white.withAlphaComponent(0.92)
        view.tintColor = .systemCyan
        view.textContainerInset = UIEdgeInsets(top: 14, left: 12, bottom: 20, right: 12)
        view.text = text
        return view
    }

    func updateUIView(_ view: UITextView, context: Context) {
        if view.text != text {
            view.text = text
        }
        guard !context.coordinator.didApplyAnchor else { return }
        context.coordinator.didApplyAnchor = true
        // Next runloop tick: the sheet has to finish presenting before the
        // text view can become first responder and show the handles.
        DispatchQueue.main.async {
            let full = NSRange(location: 0, length: (view.text as NSString).length)
            let target = anchorRange.flatMap { range in
                NSMaxRange(range) <= full.length ? range : nil
            } ?? full
            view.becomeFirstResponder()
            view.selectedRange = target
            if target != full {
                view.scrollRangeToVisible(target)
            }
        }
    }

    final class Coordinator {
        var didApplyAnchor = false
    }
}

#endif
