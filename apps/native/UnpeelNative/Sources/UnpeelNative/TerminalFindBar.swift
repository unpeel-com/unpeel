//
//  TerminalFindBar.swift
//  UnpeelNative
//
//  The ⌘F find bar overlaid on a terminal pane. Pure AppKit + plain
//  callbacks: libghostty runs the actual search (incremental match,
//  scrollback walk, highlight rendering) — this view owes it only a text
//  field and a match counter. GhosttyTerminalPane owns one and wires the
//  callbacks to the surface's search binding actions.
//

import AppKit

/// Menu-driven find commands (Edit ▸ Find). Posted by AppDelegate; the
/// currently displayed terminal pane is the one listener that acts.
extension Notification.Name {
    static let unpeelTerminalFind = Notification.Name("unpeel.terminal.find")
    static let unpeelTerminalFindNext = Notification.Name("unpeel.terminal.find-next")
    static let unpeelTerminalFindPrevious = Notification.Name("unpeel.terminal.find-previous")
}

@MainActor
final class TerminalFindBar: NSView, NSTextFieldDelegate {
    var onQueryChange: ((String) -> Void)?
    var onNext: (() -> Void)?
    var onPrevious: (() -> Void)?
    var onClose: (() -> Void)?

    private let field = NSTextField()
    private let countLabel = NSTextField(labelWithString: "")

    var query: String { field.stringValue }

    init() {
        super.init(frame: .zero)

        wantsLayer = true
        layer?.cornerRadius = 8
        layer?.cornerCurve = .continuous
        layer?.masksToBounds = true

        let background = NSVisualEffectView()
        background.material = .popover
        background.blendingMode = .withinWindow
        background.state = .followsWindowActiveState
        background.translatesAutoresizingMaskIntoConstraints = false
        addSubview(background)

        field.placeholderString = "Find"
        field.isBordered = false
        field.drawsBackground = false
        field.focusRingType = .none
        field.font = .systemFont(ofSize: NSFont.smallSystemFontSize)
        field.delegate = self
        field.translatesAutoresizingMaskIntoConstraints = false

        countLabel.font = .monospacedDigitSystemFont(
            ofSize: NSFont.smallSystemFontSize, weight: .regular
        )
        countLabel.textColor = .secondaryLabelColor
        countLabel.alignment = .right
        countLabel.setContentCompressionResistancePriority(.required, for: .horizontal)
        countLabel.translatesAutoresizingMaskIntoConstraints = false

        let previousButton = Self.symbolButton(
            "chevron.up", label: "Previous Match",
            action: #selector(previousPressed), target: self
        )
        let nextButton = Self.symbolButton(
            "chevron.down", label: "Next Match",
            action: #selector(nextPressed), target: self
        )
        let closeButton = Self.symbolButton(
            "xmark", label: "Done",
            action: #selector(closePressed), target: self
        )

        let stack = NSStackView(views: [
            field, countLabel, previousButton, nextButton, closeButton,
        ])
        stack.orientation = .horizontal
        stack.spacing = 6
        stack.edgeInsets = NSEdgeInsets(top: 6, left: 10, bottom: 6, right: 8)
        stack.translatesAutoresizingMaskIntoConstraints = false
        addSubview(stack)

        NSLayoutConstraint.activate([
            background.topAnchor.constraint(equalTo: topAnchor),
            background.leadingAnchor.constraint(equalTo: leadingAnchor),
            background.trailingAnchor.constraint(equalTo: trailingAnchor),
            background.bottomAnchor.constraint(equalTo: bottomAnchor),
            stack.topAnchor.constraint(equalTo: topAnchor),
            stack.leadingAnchor.constraint(equalTo: leadingAnchor),
            stack.trailingAnchor.constraint(equalTo: trailingAnchor),
            stack.bottomAnchor.constraint(equalTo: bottomAnchor),
            field.widthAnchor.constraint(equalToConstant: 170),
            countLabel.widthAnchor.constraint(greaterThanOrEqualToConstant: 44),
        ])
    }

    @available(*, unavailable)
    required init?(coder _: NSCoder) { fatalError("not supported") }

    private static func symbolButton(
        _ symbol: String,
        label: String,
        action: Selector,
        target: AnyObject
    ) -> NSButton {
        let image = NSImage(
            systemSymbolName: symbol, accessibilityDescription: label
        ) ?? NSImage()
        let button = NSButton(image: image, target: target, action: action)
        button.isBordered = false
        button.bezelStyle = .regularSquare
        button.imageScaling = .scaleProportionallyDown
        button.setContentHuggingPriority(.required, for: .horizontal)
        return button
    }

    func focusField() {
        window?.makeFirstResponder(field)
        field.currentEditor()?.selectAll(nil)
    }

    /// Field currently owns keyboard focus (its field editor is first
    /// responder). Used by the pane to decide where Esc/⌘F should land.
    var fieldIsFocused: Bool {
        guard let editor = field.currentEditor() else { return false }
        return window?.firstResponder === editor
    }

    /// "3 of 17" while a match is selected; bare total otherwise; "No
    /// results" for a live query with zero matches. `selected` is 0-based.
    func updateCounts(total: Int?, selected: Int?) {
        guard !field.stringValue.isEmpty, let total else {
            countLabel.stringValue = ""
            return
        }
        if total <= 0 {
            countLabel.stringValue = "No results"
        } else if let selected, selected >= 0 {
            countLabel.stringValue = "\(selected + 1) of \(total)"
        } else {
            countLabel.stringValue = "\(total)"
        }
    }

    // MARK: - NSTextFieldDelegate

    func controlTextDidChange(_: Notification) {
        onQueryChange?(field.stringValue)
    }

    func control(
        _: NSControl,
        textView _: NSTextView,
        doCommandBy commandSelector: Selector
    ) -> Bool {
        switch commandSelector {
        case #selector(NSResponder.insertNewline(_:)):
            let shift = NSApp.currentEvent?.modifierFlags.contains(.shift) ?? false
            if shift { onPrevious?() } else { onNext?() }
            return true
        case #selector(NSResponder.cancelOperation(_:)):
            onClose?()
            return true
        default:
            return false
        }
    }

    @objc private func previousPressed() { onPrevious?() }
    @objc private func nextPressed() { onNext?() }
    @objc private func closePressed() { onClose?() }
}
