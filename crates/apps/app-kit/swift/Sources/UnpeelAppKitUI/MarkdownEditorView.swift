import AppKit
import SwiftUI

private final class MarkdownInsertKeyMonitor: @unchecked Sendable {
    private let token: Any?

    init(handler: @escaping (NSEvent) -> NSEvent?) {
        token = NSEvent.addLocalMonitorForEvents(matching: .keyDown, handler: handler)
    }

    deinit {
        if let token {
            NSEvent.removeMonitor(token)
        }
    }
}

func shouldApplyAuthoritativeMarkdownSelection(
    editorOwnsFocus: Bool,
    currentRange: NSRange,
    previousRange: NSRange?,
    incomingRange: NSRange
) -> Bool {
    !editorOwnsFocus
        || currentRange == previousRange
        || currentRange == incomingRange
}

/// Native renderer for App Kit's first opinionated component.
///
/// The terminal-backed Rust App remains authoritative. This view applies its
/// latest snapshot and returns range edits, selections, save commands, and
/// presentation changes through `onAction`. The session transport adds the
/// authenticated participant, client, renderer, event, and revision envelope.
@MainActor
public struct MarkdownEditorView: View {
    public let snapshot: UISnapshot
    public let onAction: (UIAction) -> Void
    @State private var backHovered = false

    public init(snapshot: UISnapshot, onAction: @escaping (UIAction) -> Void) {
        self.snapshot = snapshot
        self.onAction = onAction
    }

    public var body: some View {
        switch snapshot.root.component {
        case .markdownEditor(let editor):
            VStack(spacing: 0) {
                toolbar(editor)
                Divider()
                editorContent(editor)
                FooterActionsView(footer: editor.footer, onAction: onAction)
            }
        case .canvasPage, .media, .menu, .page, .surface, .textBox, .tree, .unsupported:
            EmptyView()
        }
    }

    private func toolbar(_ editor: MarkdownEditorSpec) -> some View {
        HStack(spacing: 10) {
            if let back = editor.back {
                Button {
                    onAction(UIAction(nodeID: snapshot.root.id, action: back, kind: .cancel))
                } label: {
                    Image(systemName: "chevron.left")
                        .font(.body.weight(.semibold))
                        .frame(width: 24, height: 22)
                        .background(
                            RoundedRectangle(cornerRadius: 6, style: .continuous)
                                .fill(Color.primary.opacity(backHovered ? 0.14 : 0))
                        )
                        .contentShape(Rectangle())
                }
                .buttonStyle(.plain)
                .onHover { backHovered = $0 }
                .keyboardShortcut(.escape, modifiers: [])
                .accessibilityLabel("Back")
            }
            Text(editor.title ?? "Markdown")
                .font(.system(size: 12, weight: .medium))
                .lineLimit(1)
            if editor.dirty {
                Circle()
                    .fill(.secondary)
                    .frame(width: 5, height: 5)
                    .accessibilityLabel("Unsaved changes")
            }
            Spacer(minLength: 8)
            if let action = editor.actions.setPresentation {
                Picker(
                    "Presentation",
                    selection: Binding(
                        get: { editor.presentation },
                        set: { presentation in
                            onAction(
                                UIAction(
                                    nodeID: snapshot.root.id,
                                    action: action,
                                    kind: .change,
                                    value: .text(presentation.rawValue)
                                ))
                        }
                    )
                ) {
                    Text("Source").tag(MarkdownPresentation.source)
                    Text("Preview").tag(MarkdownPresentation.preview)
                    Text("Split").tag(MarkdownPresentation.split)
                }
                .labelsHidden()
                .pickerStyle(.segmented)
                .frame(width: 190)
            }
            if let action = editor.actions.openMenu {
                Button("Commands") {
                    onAction(
                        UIAction(
                            nodeID: snapshot.root.id,
                            action: action,
                            kind: .command,
                            value: .text("palette")
                        ))
                }
                .help("Open block commands")
            }
            if let action = editor.actions.save, !editor.readOnly {
                Button("Save") {
                    onAction(
                        UIAction(
                            nodeID: snapshot.root.id,
                            action: action,
                            kind: .command
                        ))
                }
                .keyboardShortcut("s", modifiers: .command)
            }
        }
        .padding(.horizontal, 10)
        .frame(height: 38)
    }

    @ViewBuilder
    private func editorContent(_ editor: MarkdownEditorSpec) -> some View {
        switch editor.presentation {
        case .source:
            sourceEditor(editor)
        case .preview:
            MarkdownPreview(text: editor.text)
        case .split:
            HSplitView {
                sourceEditor(editor)
                MarkdownPreview(text: editor.text)
            }
        }
    }

    private func sourceEditor(_ editor: MarkdownEditorSpec) -> some View {
        ZStack(alignment: .topLeading) {
            MarkdownTextView(
                nodeID: snapshot.root.id,
                revision: snapshot.revision,
                editor: editor,
                onAction: onAction
            )
            if editor.text.isEmpty, !editor.placeholder.isEmpty {
                Text(editor.placeholder)
                    .font(.system(.body, design: .monospaced))
                    .foregroundStyle(.tertiary)
                    .padding(.leading, 7)
                    .padding(.top, 8)
                    .allowsHitTesting(false)
            }
        }
    }
}

@MainActor
private struct MarkdownPreview: View {
    let text: String

    var body: some View {
        ScrollView {
            Text(rendered)
                .textSelection(.enabled)
                .frame(maxWidth: .infinity, alignment: .topLeading)
                .padding(16)
        }
    }

    private var rendered: AttributedString {
        (try? AttributedString(markdown: text)) ?? AttributedString(text)
    }
}

struct MarkdownTaskToggleEdit: Equatable {
    let range: NSRange
    let replacement: String
}

private struct MarkdownGhostHint: Equatable {
    let text: String
    let utf16Offset: Int
}

func markdownTaskToggleEdit(text: String, utf16Offset: Int) -> MarkdownTaskToggleEdit? {
    let source = text as NSString
    guard utf16Offset >= 0, utf16Offset <= source.length else { return nil }
    let location = min(utf16Offset, max(source.length - 1, 0))
    let lineRange = source.lineRange(for: NSRange(location: location, length: 0))
    let line = source.substring(with: lineRange)
    guard
        let expression = try? NSRegularExpression(
            pattern: #"^(\s*(?:(?:[-+*])|(?:\d+\.))\s+)\[([ xX])\]"#
        ),
        let match = expression.firstMatch(
            in: line,
            range: NSRange(location: 0, length: (line as NSString).length)
        )
    else { return nil }
    let markerStart = lineRange.location + match.range(at: 1).length
    let markerEnd = markerStart + 2
    guard utf16Offset >= markerStart, utf16Offset <= markerEnd else { return nil }
    let stateRange = NSRange(location: markerStart + 1, length: 1)
    let checked = source.substring(with: stateRange) != " "
    return MarkdownTaskToggleEdit(range: stateRange, replacement: checked ? " " : "x")
}

@MainActor
private final class InteractiveMarkdownTextView: NSTextView {
    var interactionEnabled = true
    var replaceFromInteraction: ((NSRange, String, Int) -> Void)?
    var commandHint: MarkdownGhostHint? {
        didSet {
            guard commandHint != oldValue else { return }
            needsDisplay = true
            setAccessibilityHelp(commandHint?.text)
        }
    }

    override func draw(_ dirtyRect: NSRect) {
        super.draw(dirtyRect)
        guard let commandHint,
            let origin = commandHintOrigin(utf16Offset: commandHint.utf16Offset)
        else { return }
        NSAttributedString(
            string: commandHint.text,
            attributes: [
                .font: font
                    ?? NSFont.monospacedSystemFont(
                        ofSize: NSFont.systemFontSize,
                        weight: .regular
                    ),
                .foregroundColor: NSColor.placeholderTextColor,
            ]
        ).draw(at: origin)
    }

    override func mouseDown(with event: NSEvent) {
        if interactionEnabled,
            event.buttonNumber == 0,
            event.clickCount == 1,
            let offset = characterOffset(at: event.locationInWindow),
            let edit = markdownTaskToggleEdit(text: string, utf16Offset: offset)
        {
            replaceFromInteraction?(edit.range, edit.replacement, 1)
            return
        }
        super.mouseDown(with: event)
    }

    override func draggingEntered(_ sender: NSDraggingInfo) -> NSDragOperation {
        interactionEnabled && droppedText(from: sender.draggingPasteboard) != nil ? .copy : []
    }

    override func performDragOperation(_ sender: NSDraggingInfo) -> Bool {
        guard interactionEnabled,
            let text = droppedText(from: sender.draggingPasteboard),
            !text.isEmpty
        else { return false }
        let offset = characterOffset(at: sender.draggingLocation) ?? selectedRange().location
        replaceFromInteraction?(
            NSRange(location: min(offset, (string as NSString).length), length: 0),
            text,
            text.utf16.count
        )
        return true
    }

    private func characterOffset(at windowPoint: NSPoint) -> Int? {
        guard let layoutManager, let textContainer else { return nil }
        let point = convert(windowPoint, from: nil)
        let containerPoint = NSPoint(
            x: point.x - textContainerOrigin.x,
            y: point.y - textContainerOrigin.y
        )
        let glyph = layoutManager.glyphIndex(
            for: containerPoint,
            in: textContainer,
            fractionOfDistanceThroughGlyph: nil
        )
        guard glyph < layoutManager.numberOfGlyphs else {
            return (string as NSString).length
        }
        return layoutManager.characterIndexForGlyph(at: glyph)
    }

    private func commandHintOrigin(utf16Offset: Int) -> NSPoint? {
        guard let layoutManager, let textContainer else { return nil }
        layoutManager.ensureLayout(for: textContainer)
        let length = (string as NSString).length
        let offset = min(max(0, utf16Offset), length)
        if length == 0 || (offset == length && string.hasSuffix("\n")) {
            let extra = layoutManager.extraLineFragmentRect
            return NSPoint(
                x: textContainerOrigin.x + extra.minX,
                y: textContainerOrigin.y + extra.minY
            )
        }
        let glyph = layoutManager.glyphIndexForCharacter(at: min(offset, length - 1))
        let line = layoutManager.lineFragmentRect(
            forGlyphAt: glyph,
            effectiveRange: nil
        )
        let location = layoutManager.location(forGlyphAt: glyph)
        return NSPoint(
            x: textContainerOrigin.x + location.x,
            y: textContainerOrigin.y + line.minY
        )
    }

    private func droppedText(from pasteboard: NSPasteboard) -> String? {
        let urls =
            pasteboard.readObjects(
                forClasses: [NSURL.self],
                options: [.urlReadingFileURLsOnly: true]
            ) as? [URL]
        let raw =
            if let urls, !urls.isEmpty {
                urls.map(\.path).joined(separator: "\n")
            } else {
                pasteboard.string(forType: .string)
            }
        return raw?
            .replacingOccurrences(of: "\0", with: "")
            .replacingOccurrences(of: "\r\n", with: "\n")
            .replacingOccurrences(of: "\r", with: "\n")
    }
}

@MainActor
private struct MarkdownTextView: NSViewRepresentable {
    let nodeID: String
    let revision: Int
    let editor: MarkdownEditorSpec
    let onAction: (UIAction) -> Void

    func makeCoordinator() -> Coordinator {
        Coordinator(
            nodeID: nodeID,
            revision: revision,
            editor: editor,
            onAction: onAction
        )
    }

    func makeNSView(context: Context) -> NSScrollView {
        let scrollView = NSScrollView()
        scrollView.hasVerticalScroller = true
        scrollView.hasHorizontalScroller = false
        scrollView.autohidesScrollers = true
        scrollView.borderType = .noBorder
        scrollView.drawsBackground = false

        let textView = InteractiveMarkdownTextView()
        textView.delegate = context.coordinator
        textView.isRichText = false
        textView.importsGraphics = false
        textView.isAutomaticQuoteSubstitutionEnabled = false
        textView.isAutomaticDashSubstitutionEnabled = false
        textView.isAutomaticTextReplacementEnabled = false
        textView.isAutomaticSpellingCorrectionEnabled = false
        textView.isVerticallyResizable = true
        textView.isHorizontallyResizable = false
        textView.autoresizingMask = [.width]
        textView.textContainer?.widthTracksTextView = true
        textView.textContainer?.containerSize = NSSize(
            width: 0,
            height: CGFloat.greatestFiniteMagnitude
        )
        textView.font = .monospacedSystemFont(ofSize: NSFont.systemFontSize, weight: .regular)
        textView.drawsBackground = false
        textView.textContainerInset = NSSize(width: 5, height: 6)
        textView.registerForDraggedTypes([.fileURL, .string])
        scrollView.documentView = textView
        context.coordinator.attach(textView)
        context.coordinator.apply(editor, revision: revision, to: textView)
        return scrollView
    }

    func updateNSView(_ scrollView: NSScrollView, context: Context) {
        guard let textView = scrollView.documentView as? NSTextView else { return }
        context.coordinator.update(
            nodeID: nodeID,
            revision: revision,
            editor: editor,
            onAction: onAction
        )
        context.coordinator.apply(editor, revision: revision, to: textView)
    }

    @MainActor
    final class Coordinator: NSObject, NSTextViewDelegate {
        private var nodeID: String
        private var editor: MarkdownEditorSpec
        private var onAction: (UIAction) -> Void
        private var applyingSnapshot = false
        private weak var textView: NSTextView?
        private var hasAppliedSnapshot = false
        private var authoritativeRevision: Int
        private var authoritativeText: String
        private var authoritativeSelection: UITextSelection
        private var inFlightText: String?
        private var flushWorkItem: DispatchWorkItem?
        private var insertPopover: NSPopover?
        private var semanticSelectedID: String?
        private var insertKeyMonitor: MarkdownInsertKeyMonitor?
        private var contextItems: [String: UIMenuItemSpec] = [:]

        init(
            nodeID: String,
            revision: Int,
            editor: MarkdownEditorSpec,
            onAction: @escaping (UIAction) -> Void
        ) {
            self.nodeID = nodeID
            self.editor = editor
            self.onAction = onAction
            authoritativeRevision = revision
            authoritativeText = editor.text
            authoritativeSelection = editor.selection
        }

        func attach(_ textView: NSTextView) {
            self.textView = textView
            if let textView = textView as? InteractiveMarkdownTextView {
                textView.replaceFromInteraction = { [weak self, weak textView] range, text, caret in
                    guard let self, let textView else { return }
                    self.replaceText(
                        in: textView,
                        range: range,
                        with: text,
                        caretUTF16Offset: caret
                    )
                }
            }
        }

        func update(
            nodeID: String,
            revision _: Int,
            editor: MarkdownEditorSpec,
            onAction: @escaping (UIAction) -> Void
        ) {
            self.nodeID = nodeID
            self.editor = editor
            self.onAction = onAction
        }

        func apply(
            _ editor: MarkdownEditorSpec,
            revision: Int,
            to textView: NSTextView
        ) {
            applyingSnapshot = true
            defer {
                applyingSnapshot = false
                configureContextMenu(for: textView)
                refreshInsertMenu(in: textView)
            }
            textView.isEditable = !editor.readOnly && editor.actions.replaceRange != nil
            textView.isSelectable = true
            (textView as? InteractiveMarkdownTextView)?.interactionEnabled = textView.isEditable
            var shouldApplyAuthoritativeSelection = false
            let previousSelection = authoritativeSelection
            let selectionChanged = editor.selection != previousSelection

            if !hasAppliedSnapshot {
                hasAppliedSnapshot = true
                authoritativeRevision = revision
                authoritativeText = editor.text
                textView.string = editor.text
                shouldApplyAuthoritativeSelection = true
            } else {
                let previousRevision = authoritativeRevision
                let previousAuthority = authoritativeText
                let localText = textView.string
                let hadLocalChanges = localText != previousAuthority
                let incomingChanged =
                    revision != previousRevision
                    || editor.text != previousAuthority

                if incomingChanged {
                    authoritativeRevision = revision
                    authoritativeText = editor.text
                    if inFlightText != nil,
                        revision > previousRevision || editor.text == inFlightText
                    {
                        inFlightText = nil
                    }
                    if !hadLocalChanges || localText == editor.text {
                        if textView.string != editor.text {
                            textView.string = editor.text
                        }
                        shouldApplyAuthoritativeSelection =
                            !hadLocalChanges
                            && editor.text != previousAuthority
                    }
                }
            }
            authoritativeSelection = editor.selection

            // Selection-only revisions are common when the terminal performs
            // a drag, double-click, or triple-click. Apply those when the
            // native editor still reflects the previous authoritative range.
            // If the user is concurrently moving the native selection, keep
            // that optimistic local range until its own event is echoed.
            if !shouldApplyAuthoritativeSelection,
                selectionChanged,
                textView.string == editor.text,
                inFlightText == nil,
                let incomingRange = Self.nsRange(for: editor.selection, in: editor.text)
            {
                let currentRange = textView.selectedRange()
                let previousRange = Self.nsRange(for: previousSelection, in: editor.text)
                let ownsFocus = textView.window?.firstResponder === textView
                shouldApplyAuthoritativeSelection = shouldApplyAuthoritativeMarkdownSelection(
                    editorOwnsFocus: ownsFocus,
                    currentRange: currentRange,
                    previousRange: previousRange,
                    incomingRange: incomingRange
                )
            }

            if shouldApplyAuthoritativeSelection,
                let range = Self.nsRange(for: editor.selection, in: editor.text),
                textView.selectedRange() != range
            {
                textView.setSelectedRange(range)
            }
            if let textView = textView as? InteractiveMarkdownTextView,
                editor.commandHintVisible,
                let range = Self.nsRange(for: editor.selection, in: editor.text)
            {
                textView.commandHint = MarkdownGhostHint(
                    text: editor.commandHint?.text ?? "",
                    utf16Offset: range.location
                )
            } else {
                (textView as? InteractiveMarkdownTextView)?.commandHint = nil
            }
            if textView.string != authoritativeText, inFlightText == nil {
                scheduleFlush(from: textView)
            }
        }

        func textView(
            _ textView: NSTextView,
            shouldChangeTextIn affectedCharRange: NSRange,
            replacementString: String?
        ) -> Bool {
            guard !applyingSnapshot, !editor.readOnly,
                editor.actions.replaceRange != nil,
                affectedCharRange.location != NSNotFound,
                NSMaxRange(affectedCharRange) <= (textView.string as NSString).length
            else { return false }

            if let action = editor.actions.openMenu,
                let replacementString,
                let trigger = editor.menuTrigger(forTextInput: replacementString)
            {
                onAction(
                    UIAction(
                        nodeID: nodeID,
                        action: action,
                        kind: .command,
                        value: .text(trigger.rawValue)
                    ))
                return false
            }

            return true
        }

        func textDidChange(_ notification: Notification) {
            guard !applyingSnapshot,
                let textView = notification.object as? NSTextView
            else { return }
            scheduleFlush(from: textView)
            refreshInsertMenu(in: textView)
        }

        func textViewDidChangeSelection(_ notification: Notification) {
            guard !applyingSnapshot,
                let textView = notification.object as? NSTextView
            else { return }
            refreshInsertMenu(in: textView)
            guard textView.string == authoritativeText, inFlightText == nil else { return }
            guard let action = editor.actions.setSelection,
                let selection = Self.textSelection(
                    for: textView.selectedRange(),
                    in: textView.string
                )
            else { return }
            onAction(
                UIAction(
                    nodeID: nodeID,
                    action: action,
                    kind: .select,
                    value: .textSelection(selection)
                ))
        }

        func textView(
            _ textView: NSTextView,
            doCommandBy commandSelector: Selector
        ) -> Bool {
            let command = NSStringFromSelector(commandSelector)
            if editor.insertMenu != nil {
                switch command {
                case "moveUp:":
                    moveInsertSelection(-1, in: textView)
                    return true
                case "moveDown:":
                    moveInsertSelection(1, in: textView)
                    return true
                case "insertNewline:", "insertNewlineIgnoringFieldEditor:", "insertTab:":
                    applySelectedInsert(in: textView)
                    return true
                case "cancelOperation:":
                    clearSlashCommand(in: textView)
                    return true
                default:
                    break
                }
            }
            if command == "deleteBackward:",
                let edit = markdownBackspaceEdit(
                    text: textView.string,
                    selection: textView.selectedRange()
                )
            {
                replaceText(
                    in: textView,
                    range: edit.lineRange,
                    with: edit.replacement,
                    caretUTF16Offset: edit.caretUTF16Offset
                )
                return true
            }
            return false
        }

        private func scheduleFlush(from textView: NSTextView) {
            flushWorkItem?.cancel()
            flushWorkItem = nil
            guard inFlightText == nil, textView.string != authoritativeText else { return }
            let work = DispatchWorkItem { [weak self, weak textView] in
                guard let self, let textView else { return }
                self.flushWorkItem = nil
                self.flushLocalEdit(from: textView)
            }
            flushWorkItem = work
            DispatchQueue.main.asyncAfter(deadline: .now() + 0.09, execute: work)
        }

        private func flushLocalEdit(from textView: NSTextView) {
            guard !applyingSnapshot, inFlightText == nil, !editor.readOnly,
                let action = editor.actions.replaceRange,
                let edit = Self.diffText(before: authoritativeText, after: textView.string)
            else { return }
            inFlightText = textView.string
            onAction(
                UIAction(
                    nodeID: nodeID,
                    action: action,
                    kind: .change,
                    value: .textEdit(edit)
                ))
        }

        private func refreshInsertMenu(in textView: NSTextView) {
            guard !editor.readOnly, let menu = editor.insertMenu else {
                closeInsertMenu()
                return
            }
            let enabled = menu.items.filter { !$0.disabled }
            if semanticSelectedID == nil
                || !enabled.contains(where: { $0.id == semanticSelectedID })
            {
                semanticSelectedID = menu.selectedID ?? enabled.first?.id
            }
            installInsertKeyMonitor(for: textView)
            showInsertPopover(relativeTo: textView)
        }

        private func showInsertPopover(relativeTo textView: NSTextView) {
            guard let semantic = editor.insertMenu else {
                closeInsertMenu()
                return
            }
            let popover: NSPopover
            if let insertPopover {
                popover = insertPopover
            } else {
                popover = NSPopover()
                popover.behavior = .applicationDefined
                popover.animates = false
                insertPopover = popover
            }
            let menu = UIMenuSpec(
                label: semantic.label,
                presentation: semantic.presentation,
                anchor: semantic.anchor,
                items: semantic.items,
                selectedID: semanticSelectedID,
                dismiss: semantic.dismiss
            )
            popover.contentViewController = NSHostingController(
                rootView:
                    SemanticMenuContent(ownerID: nodeID, menu: menu) {
                        [weak self, weak textView] action in
                        guard let self else { return }
                        self.onAction(action)
                        self.insertPopover?.performClose(nil)
                        textView?.window?.makeFirstResponder(textView)
                    }
            )
            popover.contentSize = NSSize(
                width: 238,
                height: min(330, CGFloat(max(1, semantic.items.count) * 30 + 12))
            )
            guard let window = textView.window else { return }
            if !popover.isShown {
                let screenRect = textView.firstRect(
                    forCharacterRange: textView.selectedRange(),
                    actualRange: nil
                )
                let localRect = textView.convert(window.convertFromScreen(screenRect), from: nil)
                let anchor = NSRect(
                    x: localRect.minX,
                    y: localRect.minY,
                    width: max(1, localRect.width),
                    height: max(16, localRect.height)
                )
                popover.show(relativeTo: anchor, of: textView, preferredEdge: .maxY)
            }
            // NSPopover hosts a second window. Keep all typing and menu keys
            // owned by the document rather than allowing its SwiftUI content
            // to become the first responder.
            window.makeFirstResponder(textView)
            DispatchQueue.main.async { [weak window, weak textView] in
                guard let window, let textView else { return }
                window.makeFirstResponder(textView)
            }
        }

        private func installInsertKeyMonitor(for textView: NSTextView) {
            guard insertKeyMonitor == nil else { return }
            insertKeyMonitor = MarkdownInsertKeyMonitor { [weak self, weak textView] event in
                guard let self, let textView,
                    self.editor.insertMenu != nil,
                    textView.window?.isKeyWindow == true
                else { return event }
                return self.handleInsertMenuKey(event, in: textView) ? nil : event
            }
        }

        private func handleInsertMenuKey(_ event: NSEvent, in textView: NSTextView) -> Bool {
            switch event.keyCode {
            case 126:  // Up arrow
                moveInsertSelection(-1, in: textView)
            case 125:  // Down arrow
                moveInsertSelection(1, in: textView)
            case 115:  // Home
                setInsertSelection(0, in: textView)
            case 119:  // End
                setInsertSelection(Int.max, in: textView)
            case 36, 76, 48:  // Return, keypad Enter, Tab
                applySelectedInsert(in: textView)
            case 53:  // Escape
                clearSlashCommand(in: textView)
            default:
                return false
            }
            return true
        }

        private func moveInsertSelection(_ delta: Int, in textView: NSTextView) {
            guard let menu = editor.insertMenu else { return }
            let enabled = menu.items.filter { !$0.disabled }
            guard !enabled.isEmpty else { return }
            let current = max(
                0,
                enabled.firstIndex(where: { $0.id == semanticSelectedID }) ?? 0
            )
            semanticSelectedID = enabled[(current + delta + enabled.count) % enabled.count].id
            showInsertPopover(relativeTo: textView)
        }

        private func setInsertSelection(_ index: Int, in textView: NSTextView) {
            guard let menu = editor.insertMenu else { return }
            let enabled = menu.items.filter { !$0.disabled }
            guard !enabled.isEmpty else { return }
            semanticSelectedID = enabled[min(max(0, index), enabled.count - 1)].id
            showInsertPopover(relativeTo: textView)
        }

        private func applySelectedInsert(in textView: NSTextView) {
            guard let menu = editor.insertMenu,
                let item = menu.items.first(where: { $0.id == semanticSelectedID }),
                !item.disabled
            else { return }
            onAction(UIAction(nodeID: item.id, action: item.action, kind: .activate))
            insertPopover?.performClose(nil)
            textView.window?.makeFirstResponder(textView)
        }

        private func clearSlashCommand(in textView: NSTextView) {
            guard let menu = editor.insertMenu, let dismiss = menu.dismiss else { return }
            onAction(UIAction(nodeID: nodeID, action: dismiss, kind: .cancel))
            insertPopover?.performClose(nil)
            textView.window?.makeFirstResponder(textView)
        }

        private func replaceText(
            in textView: NSTextView,
            range: NSRange,
            with replacement: String,
            caretUTF16Offset: Int
        ) {
            guard textView.shouldChangeText(in: range, replacementString: replacement) else {
                return
            }
            textView.textStorage?.replaceCharacters(in: range, with: replacement)
            textView.didChangeText()
            textView.setSelectedRange(
                NSRange(
                    location: range.location + caretUTF16Offset,
                    length: 0
                ))
        }

        private func closeInsertMenu() {
            insertPopover?.performClose(nil)
            insertPopover = nil
            insertKeyMonitor = nil
            semanticSelectedID = nil
        }

        private func configureContextMenu(for textView: NSTextView) {
            guard let semantic = editor.contextMenu else {
                textView.menu = nil
                contextItems.removeAll()
                return
            }
            let menu = NSMenu(title: semantic.label)
            contextItems = Dictionary(uniqueKeysWithValues: semantic.items.map { ($0.id, $0) })
            for item in semantic.items {
                let native = NSMenuItem(
                    title: item.label,
                    action: #selector(activateSemanticContextItem(_:)),
                    keyEquivalent: ""
                )
                native.target = self
                native.representedObject = item.id
                native.isEnabled = !item.disabled
                if item.role == .danger {
                    native.attributedTitle = NSAttributedString(
                        string: item.label,
                        attributes: [.foregroundColor: NSColor.systemRed]
                    )
                }
                menu.addItem(native)
            }
            textView.menu = menu
        }

        @objc private func activateSemanticContextItem(_ sender: NSMenuItem) {
            guard let id = sender.representedObject as? String,
                let item = contextItems[id], !item.disabled
            else { return }
            onAction(UIAction(nodeID: item.id, action: item.action, kind: .activate))
        }

        private static func diffText(before: String, after: String) -> UITextEdit? {
            guard before != after else { return nil }
            let beforeCharacters = Array(before)
            let afterCharacters = Array(after)
            var prefix = 0
            while prefix < beforeCharacters.count,
                prefix < afterCharacters.count,
                beforeCharacters[prefix] == afterCharacters[prefix]
            {
                prefix += 1
            }
            var suffix = 0
            while suffix < beforeCharacters.count - prefix,
                suffix < afterCharacters.count - prefix,
                beforeCharacters[beforeCharacters.count - suffix - 1]
                    == afterCharacters[afterCharacters.count - suffix - 1]
            {
                suffix += 1
            }
            let start = String(beforeCharacters[..<prefix]).utf16.count
            let removedEnd = String(
                beforeCharacters[..<(beforeCharacters.count - suffix)]
            ).utf16.count
            let replacement = String(
                afterCharacters[prefix..<(afterCharacters.count - suffix)]
            )
            return UITextEdit(
                range: UITextRange(
                    start: position(atUTF16Offset: start, in: before),
                    end: position(atUTF16Offset: removedEnd, in: before)
                ),
                text: replacement
            )
        }

        private static func textRange(for range: NSRange, in text: String) -> UITextRange? {
            guard range.location != NSNotFound,
                range.location >= 0,
                NSMaxRange(range) <= (text as NSString).length
            else { return nil }
            return UITextRange(
                start: position(atUTF16Offset: range.location, in: text),
                end: position(atUTF16Offset: NSMaxRange(range), in: text)
            )
        }

        private static func textSelection(
            for range: NSRange,
            in text: String
        ) -> UITextSelection? {
            guard let range = textRange(for: range, in: text) else { return nil }
            return UITextSelection(anchor: range.start, head: range.end)
        }

        private static func nsRange(for selection: UITextSelection, in text: String) -> NSRange? {
            guard let anchor = utf16Offset(for: selection.anchor, in: text),
                let head = utf16Offset(for: selection.head, in: text)
            else { return nil }
            return NSRange(location: min(anchor, head), length: abs(head - anchor))
        }

        private static func position(atUTF16Offset target: Int, in text: String) -> UITextPosition {
            let utf16 = text.utf16
            let clamped = max(0, min(target, utf16.count))
            var line = 0
            var lineStart = 0
            for (offset, unit) in utf16.prefix(clamped).enumerated() where unit == 10 {
                line += 1
                lineStart = offset + 1
            }
            return UITextPosition(line: line, utf16Column: clamped - lineStart)
        }

        private static func utf16Offset(for position: UITextPosition, in text: String) -> Int? {
            guard position.line >= 0, position.utf16Column >= 0 else { return nil }
            var line = 0
            var lineStart = 0
            let utf16 = Array(text.utf16)
            if position.line > 0 {
                for (offset, unit) in utf16.enumerated() where unit == 10 {
                    line += 1
                    lineStart = offset + 1
                    if line == position.line { break }
                }
                guard line == position.line else { return nil }
            }
            let lineEnd = utf16[lineStart...].firstIndex(of: 10) ?? utf16.endIndex
            let target = lineStart + position.utf16Column
            guard target <= lineEnd else { return nil }
            return target
        }
    }
}
