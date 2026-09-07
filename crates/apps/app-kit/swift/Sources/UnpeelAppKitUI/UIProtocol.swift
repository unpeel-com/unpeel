import Foundation

public enum UnpeelUIProtocol {
    public static let name = "unpeel.ui"
    public static let minimumVersion = 1
    public static let maximumVersion = 1
    public static let version = maximumVersion
    public static let deltaCapability = "serverDelta"
    public static let markdownEditorCapability = "markdownEditor"
    public static let markdownCommandHintCapability = "markdownCommandHint"
    public static let menuCapability = "menu"
    public static let menuAnchorCapability = "menuAnchor"
    public static let mediaCapability = "media"
    public static let pageCapability = "page"
    public static let listCapability = "list"
    public static let listItemCapability = "listItem"
    public static let listItemMetadataCapability = "listItemMetadata"
    public static let listItemActivateCapability = "listItemActivate"
    public static let listItemPresentationCapability = "listItemPresentation"
    public static let listItemStyledTextCapability = "listItemStyledText"
    public static let listItemRoleCapability = "listItemRole"
    public static let listSelectionCapability = "listSelection"
    public static let statusSymbolCapability = "statusSymbol"
    public static let badgeCapability = "badge"
    public static let sparklineCapability = "sparkline"
    public static let barChartCapability = "barChart"
    public static let lineChartCapability = "lineChart"
    public static let gaugeCapability = "gauge"
    public static let toggleCapability = "toggle"
    public static let inputCapability = "input"
    public static let buttonCapability = "button"
    public static let pageBackCapability = "pageBack"
    public static let footerActionsCapability = "footerActions"
    public static let contentCapability = "content"
    public static let contentSelectionCapability = "contentSelection"
    public static let surfaceCapability = "surface"
    public static let canvasPageCapability = "canvasPage"
    public static let treeCapability = "tree"
    public static let treeHierarchyCapability = "treeHierarchy"
    public static let treeFilterCapability = "treeFilter"
    public static let treeParentCapability = "treeParent"
    public static let textBoxCapability = "textBox"
    /// Components renderable without an injected Host-owned presenter.
    /// A Host adds `surfaceCapability` only after wiring its authorized USRF
    /// route to unpeel-surface's local-GPU presenter.
    public static let supportedComponentCapabilities = [
        markdownEditorCapability,
        textBoxCapability,
        markdownCommandHintCapability,
        menuCapability,
        menuAnchorCapability,
        mediaCapability,
        pageCapability,
        listCapability,
        listItemCapability,
        listItemMetadataCapability,
        listItemActivateCapability,
        listItemPresentationCapability,
        listItemStyledTextCapability,
        listItemRoleCapability,
        listSelectionCapability,
        statusSymbolCapability,
        badgeCapability,
        sparklineCapability,
        barChartCapability,
        lineChartCapability,
        gaugeCapability,
        toggleCapability,
        inputCapability,
        buttonCapability,
        pageBackCapability,
        footerActionsCapability,
        contentCapability,
        contentSelectionCapability,
        treeCapability,
        treeHierarchyCapability,
        treeFilterCapability,
        treeParentCapability,
    ]
    private static let maximumWireVersion = Int(UInt32.max)

    public static func supports(_ version: Int) -> Bool {
        (minimumVersion...maximumVersion).contains(version)
    }

    public static func negotiate(minimum: Int, maximum: Int) -> Int? {
        guard minimum > 0, minimum <= maximum, maximum <= maximumWireVersion else { return nil }
        let sharedMinimum = Swift.max(minimum, minimumVersion)
        let sharedMaximum = Swift.min(maximum, maximumVersion)
        return sharedMinimum <= sharedMaximum ? sharedMaximum : nil
    }
}

public struct AppMetadata: Codable, Equatable, Sendable {
    public let id: String
    public let name: String
    public let version: String
    public let description: String?

    public init(id: String, name: String, version: String, description: String? = nil) {
        self.id = id
        self.name = name
        self.version = version
        self.description = description
    }
}

public enum UIParticipantKind: String, Codable, Equatable, Sendable {
    case human
    case agent
    case service
}

/// Opaque Host identity and signed access grants. Agents use this same type.
public struct UIParticipant: Codable, Equatable, Sendable {
    public let id: String
    public let kind: UIParticipantKind
    public let sourceSessionID: String?
    public let displayName: String?
    public let color: String?
    public let grants: [String]

    public init(
        id: String,
        kind: UIParticipantKind = .human,
        sourceSessionID: String? = nil,
        displayName: String? = nil,
        color: String? = nil,
        grants: [String] = []
    ) {
        self.id = id
        self.kind = kind
        self.sourceSessionID = sourceSessionID
        self.displayName = displayName
        self.color = color
        self.grants = grants
    }

    enum CodingKeys: String, CodingKey {
        case id
        case kind
        case sourceSessionID = "sourceSessionId"
        case displayName
        case color
        case grants
    }

    public init(from decoder: Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        id = try container.decode(String.self, forKey: .id)
        kind = try container.decodeIfPresent(UIParticipantKind.self, forKey: .kind) ?? .human
        sourceSessionID = try container.decodeIfPresent(String.self, forKey: .sourceSessionID)
        displayName = try container.decodeIfPresent(String.self, forKey: .displayName)
        color = try container.decodeIfPresent(String.self, forKey: .color)
        grants = try container.decodeIfPresent([String].self, forKey: .grants) ?? []
    }
}

public struct UIRendererMetadata: Codable, Equatable, Sendable {
    public let id: String
    public let kind: String
    public let capabilities: [String]

    public init(id: String, kind: String, capabilities: [String] = []) {
        self.id = id
        self.kind = kind
        self.capabilities = capabilities
    }

    enum CodingKeys: String, CodingKey {
        case id
        case kind
        case capabilities
    }

    public init(from decoder: Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        id = try container.decode(String.self, forKey: .id)
        kind = try container.decode(String.self, forKey: .kind)
        capabilities = try container.decodeIfPresent(
            [String].self,
            forKey: .capabilities
        ) ?? []
    }
}

public struct UIRendererState: Codable, Equatable, Sendable {
    public let rendererVisible: Bool
    public let terminalVisible: Bool

    public init(rendererVisible: Bool, terminalVisible: Bool) {
        self.rendererVisible = rendererVisible
        self.terminalVisible = terminalVisible
    }

    public static let terminal = Self(rendererVisible: false, terminalVisible: true)
    public static let component = Self(rendererVisible: true, terminalVisible: false)
    public static let hidden = Self(rendererVisible: false, terminalVisible: false)
}

/// Scoped local attachment. Never expose `participantToken` to web code.
public struct UIAttach: Codable, Equatable, Sendable, CustomDebugStringConvertible {
    public let protocolName: String
    public let minProtocolVersion: Int
    public let maxProtocolVersion: Int
    public let participantToken: String
    public let clientID: String
    public let renderer: UIRendererMetadata
    public let viewID: String
    public let expectedAppInstanceID: String?
    public let lastSeenRevision: Int?
    public let state: UIRendererState

    public init(
        participantToken: String,
        clientID: String,
        renderer: UIRendererMetadata,
        viewID: String,
        minProtocolVersion: Int = UnpeelUIProtocol.minimumVersion,
        maxProtocolVersion: Int = UnpeelUIProtocol.maximumVersion,
        expectedAppInstanceID: String? = nil,
        lastSeenRevision: Int? = nil,
        state: UIRendererState = .terminal
    ) {
        protocolName = UnpeelUIProtocol.name
        self.minProtocolVersion = minProtocolVersion
        self.maxProtocolVersion = maxProtocolVersion
        self.participantToken = participantToken
        self.clientID = clientID
        self.renderer = renderer
        self.viewID = viewID
        self.expectedAppInstanceID = expectedAppInstanceID
        self.lastSeenRevision = lastSeenRevision
        self.state = state
    }

    public var debugDescription: String {
        "UIAttach(client: \(clientID), participantToken: [REDACTED])"
    }

    enum CodingKeys: String, CodingKey {
        case protocolName = "protocol"
        case minProtocolVersion
        case maxProtocolVersion
        case participantToken
        case clientID = "clientId"
        case renderer
        case viewID = "viewId"
        case expectedAppInstanceID = "expectedAppInstanceId"
        case lastSeenRevision
        case state
    }

    public init(from decoder: Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        protocolName = try container.decode(String.self, forKey: .protocolName)
        minProtocolVersion = try container.decode(Int.self, forKey: .minProtocolVersion)
        maxProtocolVersion = try container.decode(Int.self, forKey: .maxProtocolVersion)
        guard minProtocolVersion > 0,
              minProtocolVersion <= maxProtocolVersion,
              maxProtocolVersion <= Int(UInt32.max)
        else {
            throw DecodingError.dataCorruptedError(
                forKey: .minProtocolVersion,
                in: container,
                debugDescription: "Invalid UI protocol version range"
            )
        }
        participantToken = try container.decode(String.self, forKey: .participantToken)
        guard !participantToken.isEmpty, participantToken.utf8.count <= 16_384 else {
            throw DecodingError.dataCorruptedError(
                forKey: .participantToken,
                in: container,
                debugDescription: "participantToken must contain 1...16384 bytes"
            )
        }
        clientID = try container.decode(String.self, forKey: .clientID)
        renderer = try container.decode(UIRendererMetadata.self, forKey: .renderer)
        viewID = try container.decode(String.self, forKey: .viewID)
        expectedAppInstanceID = try container.decodeIfPresent(
            String.self,
            forKey: .expectedAppInstanceID
        )
        lastSeenRevision = try container.decodeIfPresent(Int.self, forKey: .lastSeenRevision)
        state = try container.decodeIfPresent(UIRendererState.self, forKey: .state) ?? .terminal
    }
}

public struct UIAttached: Codable, Equatable, Sendable {
    public let protocolName: String
    public let protocolVersion: Int
    public let minProtocolVersion: Int
    public let maxProtocolVersion: Int
    public let app: AppMetadata
    public let appInstanceID: String
    public let participantID: String
    public let clientID: String
    public let rendererID: String
    public let viewID: String
    public let resumed: Bool
    public let currentRevision: Int?

    enum CodingKeys: String, CodingKey {
        case protocolName = "protocol"
        case protocolVersion
        case minProtocolVersion
        case maxProtocolVersion
        case app
        case appInstanceID = "appInstanceId"
        case participantID = "participantId"
        case clientID = "clientId"
        case rendererID = "rendererId"
        case viewID = "viewId"
        case resumed
        case currentRevision
    }

    public init(from decoder: Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        protocolName = try container.decode(String.self, forKey: .protocolName)
        protocolVersion = try container.decode(Int.self, forKey: .protocolVersion)
        minProtocolVersion = try container.decode(Int.self, forKey: .minProtocolVersion)
        maxProtocolVersion = try container.decode(Int.self, forKey: .maxProtocolVersion)
        guard minProtocolVersion > 0,
              minProtocolVersion <= maxProtocolVersion,
              maxProtocolVersion <= Int(UInt32.max),
              (minProtocolVersion...maxProtocolVersion).contains(protocolVersion)
        else {
            throw DecodingError.dataCorruptedError(
                forKey: .protocolVersion,
                in: container,
                debugDescription: "Selected UI protocol version is outside the server range"
            )
        }
        app = try container.decode(AppMetadata.self, forKey: .app)
        appInstanceID = try container.decode(String.self, forKey: .appInstanceID)
        participantID = try container.decode(String.self, forKey: .participantID)
        clientID = try container.decode(String.self, forKey: .clientID)
        rendererID = try container.decode(String.self, forKey: .rendererID)
        viewID = try container.decode(String.self, forKey: .viewID)
        resumed = try container.decode(Bool.self, forKey: .resumed)
        currentRevision = try container.decodeIfPresent(Int.self, forKey: .currentRevision)
    }
}

public struct UITextPosition: Codable, Equatable, Comparable, Sendable {
    public let line: Int
    public let utf16Column: Int

    public init(line: Int, utf16Column: Int) {
        self.line = line
        self.utf16Column = utf16Column
    }

    public static func < (lhs: Self, rhs: Self) -> Bool {
        (lhs.line, lhs.utf16Column) < (rhs.line, rhs.utf16Column)
    }
}

public struct UITextRange: Codable, Equatable, Sendable {
    public let start: UITextPosition
    public let end: UITextPosition

    public init(start: UITextPosition, end: UITextPosition) {
        self.start = start
        self.end = end
    }
}

public struct UITextSelection: Codable, Equatable, Sendable {
    public let anchor: UITextPosition
    public let head: UITextPosition

    public init(anchor: UITextPosition, head: UITextPosition) {
        self.anchor = anchor
        self.head = head
    }

    public static func caret(_ position: UITextPosition) -> Self {
        Self(anchor: position, head: position)
    }
}

public struct UITextEdit: Codable, Equatable, Sendable {
    public let range: UITextRange
    public let text: String

    public init(range: UITextRange, text: String) {
        self.range = range
        self.text = text
    }
}

public enum MarkdownPresentation: String, Codable, Equatable, Sendable {
    case source
    case preview
    case split
}

public enum UIMenuItemRole: String, Codable, Equatable, Hashable, Sendable {
    case standard = "default"
    case danger
}

public enum UIMenuAnchor: String, Codable, Equatable, Hashable, Sendable {
    case control
    case caret
    case pointer
}

public enum UIMenuPresentation: String, Codable, Equatable, Hashable, Sendable {
    case popup
    case context
}

public struct UIMenuItemSpec: Codable, Equatable, Hashable, Identifiable, Sendable {
    public let id: String
    public let label: String
    public let action: String
    public let hint: String?
    public let disabled: Bool
    public let role: UIMenuItemRole

    public init(
        id: String,
        label: String,
        action: String,
        hint: String? = nil,
        disabled: Bool = false,
        role: UIMenuItemRole = .standard
    ) {
        self.id = id
        self.label = label
        self.action = action
        self.hint = hint
        self.disabled = disabled
        self.role = role
    }

    enum CodingKeys: String, CodingKey { case id, label, action, hint, disabled, role }

    public init(from decoder: Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        id = try container.decode(String.self, forKey: .id)
        label = try container.decode(String.self, forKey: .label)
        action = try container.decode(String.self, forKey: .action)
        hint = try container.decodeIfPresent(String.self, forKey: .hint)
        disabled = try container.decodeIfPresent(Bool.self, forKey: .disabled) ?? false
        role = try container.decodeIfPresent(UIMenuItemRole.self, forKey: .role) ?? .standard
    }
}

public struct UIMenuSpec: Codable, Equatable, Hashable, Sendable {
    public let label: String
    public let presentation: UIMenuPresentation
    public let anchor: UIMenuAnchor
    public let items: [UIMenuItemSpec]
    public var selectedID: String?
    public let dismiss: String?

    public init(
        label: String,
        presentation: UIMenuPresentation = .popup,
        anchor: UIMenuAnchor = .control,
        items: [UIMenuItemSpec],
        selectedID: String? = nil,
        dismiss: String? = nil
    ) {
        self.label = label
        self.presentation = presentation
        self.anchor = anchor
        self.items = items
        self.selectedID = selectedID
        self.dismiss = dismiss
    }

    enum CodingKeys: String, CodingKey {
        case label, presentation, anchor, items, selectedID = "selectedId", dismiss
    }

    public init(from decoder: Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        label = try container.decode(String.self, forKey: .label)
        presentation = try container.decodeIfPresent(
            UIMenuPresentation.self,
            forKey: .presentation
        ) ?? .popup
        anchor = try container.decodeIfPresent(UIMenuAnchor.self, forKey: .anchor) ?? .control
        items = try container.decode([UIMenuItemSpec].self, forKey: .items)
        selectedID = try container.decodeIfPresent(String.self, forKey: .selectedID)
        dismiss = try container.decodeIfPresent(String.self, forKey: .dismiss)
    }

    public var requiredCapabilities: [String]? {
        let ids = Set(items.map(\.id))
        guard items.count <= 256, ids.count == items.count,
              selectedID.map(ids.contains) ?? true,
              !items.contains(where: { $0.id == selectedID && $0.disabled })
        else { return nil }
        return [UnpeelUIProtocol.menuCapability, UnpeelUIProtocol.menuAnchorCapability]
    }
}

public enum MarkdownCommandHintVisibility: String, Codable, Equatable, Sendable {
    case cursorOnEmptyLineOutsideCodeFence
}

public struct MarkdownCommandHint: Codable, Equatable, Sendable {
    public let text: String
    public let visibility: MarkdownCommandHintVisibility

    public init(
        text: String,
        visibility: MarkdownCommandHintVisibility = .cursorOnEmptyLineOutsideCodeFence
    ) {
        self.text = text
        self.visibility = visibility
    }

    var isValid: Bool {
        !text.isEmpty
            && text.utf8.count <= 4_096
            && !text.contains(where: { $0 == "\0" || $0 == "\r" || $0 == "\n" })
    }
}

public enum MarkdownMenuTrigger: String, Codable, Equatable, Sendable {
    case slash
    case palette
}

public struct MarkdownEditorActions: Codable, Equatable, Sendable {
    public let replaceRange: String?
    public let setSelection: String?
    public let save: String?
    public let undo: String?
    public let redo: String?
    public let setPresentation: String?
    public let openMenu: String?

    public init(
        replaceRange: String? = "replace-range",
        setSelection: String? = "set-selection",
        save: String? = "save",
        undo: String? = "undo",
        redo: String? = "redo",
        setPresentation: String? = "set-presentation",
        openMenu: String? = nil
    ) {
        self.replaceRange = replaceRange
        self.setSelection = setSelection
        self.save = save
        self.undo = undo
        self.redo = redo
        self.setPresentation = setPresentation
        self.openMenu = openMenu
    }
}

public struct MarkdownEditorSpec: Codable, Equatable, Sendable {
    public let text: String
    public let selection: UITextSelection
    public let presentation: MarkdownPresentation
    public let readOnly: Bool
    public let dirty: Bool
    public let placeholder: String
    public let commandHint: MarkdownCommandHint?
    public let title: String?
    public let back: String?
    public let actions: MarkdownEditorActions
    public let insertMenu: UIMenuSpec?
    public let contextMenu: UIMenuSpec?
    public let footer: UIFooterActionsSpec

    public init(
        text: String,
        selection: UITextSelection,
        presentation: MarkdownPresentation = .source,
        readOnly: Bool = false,
        dirty: Bool = false,
        placeholder: String = "",
        commandHint: MarkdownCommandHint? = nil,
        title: String? = nil,
        back: String? = nil,
        actions: MarkdownEditorActions = .init(),
        insertMenu: UIMenuSpec? = nil,
        contextMenu: UIMenuSpec? = nil,
        footer: UIFooterActionsSpec = .init()
    ) {
        self.text = text
        self.selection = selection
        self.presentation = presentation
        self.readOnly = readOnly
        self.dirty = dirty
        self.placeholder = placeholder
        self.commandHint = commandHint
        self.title = title
        self.back = back
        self.actions = actions
        self.insertMenu = insertMenu
        self.contextMenu = contextMenu
        self.footer = footer
    }

    enum CodingKeys: String, CodingKey {
        case text
        case selection
        case presentation
        case readOnly
        case dirty
        case placeholder
        case commandHint
        case title
        case back
        case actions
        case insertMenu
        case contextMenu
        case footer
    }

    public init(from decoder: Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        text = try container.decode(String.self, forKey: .text)
        selection = try container.decode(UITextSelection.self, forKey: .selection)
        presentation = try container.decodeIfPresent(
            MarkdownPresentation.self,
            forKey: .presentation
        ) ?? .source
        readOnly = try container.decodeIfPresent(Bool.self, forKey: .readOnly) ?? false
        dirty = try container.decodeIfPresent(Bool.self, forKey: .dirty) ?? false
        placeholder = try container.decodeIfPresent(String.self, forKey: .placeholder) ?? ""
        commandHint = try container.decodeIfPresent(
            MarkdownCommandHint.self,
            forKey: .commandHint
        )
        title = try container.decodeIfPresent(String.self, forKey: .title)
        back = try container.decodeIfPresent(String.self, forKey: .back)
        actions = try container.decodeIfPresent(
            MarkdownEditorActions.self,
            forKey: .actions
        ) ?? .init()
        insertMenu = try container.decodeIfPresent(UIMenuSpec.self, forKey: .insertMenu)
        contextMenu = try container.decodeIfPresent(UIMenuSpec.self, forKey: .contextMenu)
        footer = try container.decodeIfPresent(UIFooterActionsSpec.self, forKey: .footer) ?? .init()
    }

    /// Pure interpretation of the closed Rust visibility rule.
    public var commandHintVisible: Bool {
        guard let commandHint, commandHint.isValid,
              presentation != .preview,
              selection.anchor == selection.head,
              insertMenu == nil,
              !(text.isEmpty && !placeholder.isEmpty)
        else { return false }
        let lines = text.components(separatedBy: "\n")
        let line = selection.head.line
        guard lines.indices.contains(line), lines[line].isEmpty else { return false }
        switch commandHint.visibility {
        case .cursorOnEmptyLineOutsideCodeFence:
            var insideFence = false
            for index in 0...line {
                if lines[index].trimmingCharacters(in: .whitespaces).hasPrefix("```") {
                    if index == line { return false }
                    insideFence.toggle()
                }
            }
            return !insideFence
        }
    }

    /// Closed text-input triggers for the App-owned Menu action. The App
    /// reducer, not this renderer, decides whether the current line is eligible.
    public func menuTrigger(forTextInput input: String) -> MarkdownMenuTrigger? {
        guard !readOnly, insertMenu == nil, actions.openMenu != nil else { return nil }
        switch input {
        case "/": return .slash
        case "\\": return .palette
        default: return nil
        }
    }
}

public enum MediaFit: String, Codable, Equatable, Hashable, Sendable {
    case contain
    case cover
    case fill
}

public struct MediaPixelSize: Codable, Equatable, Hashable, Sendable {
    public let w: Int
    public let h: Int

    public init(w: Int, h: Int) {
        self.w = w
        self.h = h
    }

    public init(from decoder: Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        w = try container.decode(Int.self, forKey: .w)
        h = try container.decode(Int.self, forKey: .h)
        guard (1...Int(UInt32.max)).contains(w), (1...Int(UInt32.max)).contains(h) else {
            throw DecodingError.dataCorruptedError(
                forKey: .w,
                in: container,
                debugDescription: "Media intrinsic dimensions must be positive UInt32 values"
            )
        }
    }

    enum CodingKeys: String, CodingKey {
        case w
        case h
    }
}

public struct MediaCellSize: Codable, Equatable, Hashable, Sendable {
    public let w: Int?
    public let h: Int?

    public init(w: Int? = nil, h: Int? = nil) {
        self.w = w
        self.h = h
    }

    public init(from decoder: Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        w = try container.decodeIfPresent(Int.self, forKey: .w)
        h = try container.decodeIfPresent(Int.self, forKey: .h)
        guard w != nil || h != nil,
              w.map({ (1...Int(UInt16.max)).contains($0) }) ?? true,
              h.map({ (1...Int(UInt16.max)).contains($0) }) ?? true
        else {
            throw DecodingError.dataCorruptedError(
                forKey: .w,
                in: container,
                debugDescription: "Media cell size needs at least one positive UInt16 axis"
            )
        }
    }

    enum CodingKeys: String, CodingKey {
        case w
        case h
    }
}

public struct MediaPointSize: Codable, Equatable, Hashable, Sendable {
    public let w: Int?
    public let h: Int?

    public init(w: Int? = nil, h: Int? = nil) {
        self.w = w
        self.h = h
    }

    public init(from decoder: Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        w = try container.decodeIfPresent(Int.self, forKey: .w)
        h = try container.decodeIfPresent(Int.self, forKey: .h)
        guard w != nil || h != nil,
              w.map({ (1...Int(UInt32.max)).contains($0) }) ?? true,
              h.map({ (1...Int(UInt32.max)).contains($0) }) ?? true
        else {
            throw DecodingError.dataCorruptedError(
                forKey: .w,
                in: container,
                debugDescription: "Media point size needs at least one positive UInt32 axis"
            )
        }
    }

    enum CodingKeys: String, CodingKey {
        case w
        case h
    }
}

public struct MediaBlobReference: Codable, Equatable, Hashable, Sendable {
    public let sha256: String
    public let mediaType: String
    public let byteLength: Int

    public init(sha256: String, mediaType: String, byteLength: Int) {
        self.sha256 = sha256
        self.mediaType = mediaType
        self.byteLength = byteLength
    }
}

public enum MediaSource: Equatable, Hashable, Sendable {
    case path(String)
    case inline(mediaType: String, base64: String)
    case blob(MediaBlobReference)
}

extension MediaSource: Codable {
    enum CodingKeys: String, CodingKey {
        case kind
        case path
        case mediaType
        case base64
        case sha256
        case byteLength
    }

    enum Kind: String, Codable {
        case path
        case inline
        case blob
    }

    public init(from decoder: Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        switch try container.decode(Kind.self, forKey: .kind) {
        case .path:
            let path = try container.decode(String.self, forKey: .path)
            guard !path.isEmpty, path.utf8.count <= 4_096, !path.contains("\0") else {
                throw DecodingError.dataCorruptedError(
                    forKey: .path,
                    in: container,
                    debugDescription: "Media path must contain 1...4096 non-NUL bytes"
                )
            }
            self = .path(path)
        case .inline:
            let mediaType = try container.decode(String.self, forKey: .mediaType)
            let base64 = try container.decode(String.self, forKey: .base64)
            try Self.validateMediaType(mediaType, key: .mediaType, in: container)
            guard base64.utf8.count <= 349_528,
                  let data = Data(base64Encoded: base64),
                  data.base64EncodedString() == base64,
                  (1...262_144).contains(data.count)
            else {
                throw DecodingError.dataCorruptedError(
                    forKey: .base64,
                    in: container,
                    debugDescription: "Inline Media must be valid base64 containing at most 256 KiB"
                )
            }
            self = .inline(mediaType: mediaType, base64: base64)
        case .blob:
            let reference = MediaBlobReference(
                sha256: try container.decode(String.self, forKey: .sha256),
                mediaType: try container.decode(String.self, forKey: .mediaType),
                byteLength: try container.decode(Int.self, forKey: .byteLength)
            )
            try Self.validateMediaType(reference.mediaType, key: .mediaType, in: container)
            guard reference.sha256.count == 64,
                  reference.sha256.utf8.allSatisfy({
                      (48...57).contains($0) || (97...102).contains($0)
                  }),
                  (1...9_007_199_254_740_991).contains(reference.byteLength)
            else {
                throw DecodingError.dataCorruptedError(
                    forKey: .sha256,
                    in: container,
                    debugDescription: "Media blob metadata is invalid"
                )
            }
            self = .blob(reference)
        }
    }

    public func encode(to encoder: Encoder) throws {
        var container = encoder.container(keyedBy: CodingKeys.self)
        switch self {
        case let .path(path):
            try container.encode(Kind.path, forKey: .kind)
            try container.encode(path, forKey: .path)
        case let .inline(mediaType, base64):
            try container.encode(Kind.inline, forKey: .kind)
            try container.encode(mediaType, forKey: .mediaType)
            try container.encode(base64, forKey: .base64)
        case let .blob(reference):
            try container.encode(Kind.blob, forKey: .kind)
            try container.encode(reference.sha256, forKey: .sha256)
            try container.encode(reference.mediaType, forKey: .mediaType)
            try container.encode(reference.byteLength, forKey: .byteLength)
        }
    }

    private static func validateMediaType<Key: CodingKey>(
        _ value: String,
        key: Key,
        in container: KeyedDecodingContainer<Key>
    ) throws {
        let punctuation = Set("!#$&^_.+-/".utf8)
        guard value.hasPrefix("image/"),
              value.utf8.count <= 127,
              value.utf8.allSatisfy({
                  (48...57).contains($0)
                      || (65...90).contains($0)
                      || (97...122).contains($0)
                      || punctuation.contains($0)
              })
        else {
            throw DecodingError.dataCorruptedError(
                forKey: key,
                in: container,
                debugDescription: "Media mediaType must be an image MIME type"
            )
        }
    }
}

public struct MediaSpec: Codable, Equatable, Hashable, Sendable {
    public let source: MediaSource
    public let intrinsic: MediaPixelSize
    public let cells: MediaCellSize?
    public let points: MediaPointSize?
    public let fit: MediaFit
    public let alt: String
    public let activate: String?

    public init(
        source: MediaSource,
        intrinsic: MediaPixelSize,
        cells: MediaCellSize? = nil,
        points: MediaPointSize? = nil,
        fit: MediaFit = .contain,
        alt: String,
        activate: String? = nil
    ) {
        self.source = source
        self.intrinsic = intrinsic
        self.cells = cells
        self.points = points
        self.fit = fit
        self.alt = alt
        self.activate = activate
    }

    enum CodingKeys: String, CodingKey {
        case source
        case intrinsic
        case cells
        case points
        case fit
        case alt
        case activate
    }

    public init(from decoder: Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        source = try container.decode(MediaSource.self, forKey: .source)
        intrinsic = try container.decode(MediaPixelSize.self, forKey: .intrinsic)
        cells = try container.decodeIfPresent(MediaCellSize.self, forKey: .cells)
        points = try container.decodeIfPresent(MediaPointSize.self, forKey: .points)
        fit = try container.decodeIfPresent(MediaFit.self, forKey: .fit) ?? .contain
        alt = try container.decode(String.self, forKey: .alt)
        activate = try container.decodeIfPresent(String.self, forKey: .activate)
        guard alt.utf8.count <= 16_384 else {
            throw DecodingError.dataCorruptedError(
                forKey: .alt,
                in: container,
                debugDescription: "Media alt text must contain at most 16384 bytes"
            )
        }
        if let activate {
            guard !activate.isEmpty,
                  activate.utf8.count <= 256,
                  activate.utf8.allSatisfy({
                      (48...57).contains($0)
                          || (65...90).contains($0)
                          || (97...122).contains($0)
                          || [46, 95, 58, 47, 45].contains($0)
                  })
            else {
                throw DecodingError.dataCorruptedError(
                    forKey: .activate,
                    in: container,
                    debugDescription: "Media activate must be a portable action identifier"
                )
            }
        }
    }

    public var resolvedPointSize: (w: Int, h: Int) {
        guard let points else { return (intrinsic.w, intrinsic.h) }
        switch (points.w, points.h) {
        case let (w?, h?):
            return (w, h)
        case let (w?, nil):
            return (w, ratioCeil(w, intrinsic.h, intrinsic.w))
        case let (nil, h?):
            return (ratioCeil(h, intrinsic.w, intrinsic.h), h)
        case (nil, nil):
            return (intrinsic.w, intrinsic.h)
        }
    }
}

/// Opaque route resolved by the existing authenticated Host. These are not
/// socket paths, URLs, credentials, or USRF header fields.
public struct SurfaceReference: Codable, Equatable, Hashable, Sendable {
    public let sessionID: String
    public let streamID: String

    public init(sessionID: String, streamID: String) {
        self.sessionID = sessionID
        self.streamID = streamID
    }

    enum CodingKeys: String, CodingKey {
        case sessionID = "sessionId"
        case streamID = "streamId"
    }
}

public struct SurfaceCellSize: Codable, Equatable, Hashable, Sendable {
    public let w: Int?
    public let h: Int?

    public init(w: Int? = nil, h: Int? = nil) {
        self.w = w
        self.h = h
    }

    public init(from decoder: Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        w = try container.decodeIfPresent(Int.self, forKey: .w)
        h = try container.decodeIfPresent(Int.self, forKey: .h)
        guard w != nil || h != nil,
              w.map({ (1...Int(UInt16.max)).contains($0) }) ?? true,
              h.map({ (1...Int(UInt16.max)).contains($0) }) ?? true
        else {
            throw DecodingError.dataCorruptedError(
                forKey: .w,
                in: container,
                debugDescription: "Surface cell size needs at least one positive UInt16 axis"
            )
        }
    }

    enum CodingKeys: String, CodingKey { case w, h }
}

public struct SurfacePointSize: Codable, Equatable, Hashable, Sendable {
    public let w: Int?
    public let h: Int?

    public init(w: Int? = nil, h: Int? = nil) {
        self.w = w
        self.h = h
    }

    public init(from decoder: Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        w = try container.decodeIfPresent(Int.self, forKey: .w)
        h = try container.decodeIfPresent(Int.self, forKey: .h)
        guard w != nil || h != nil,
              w.map({ (1...Int(UInt32.max)).contains($0) }) ?? true,
              h.map({ (1...Int(UInt32.max)).contains($0) }) ?? true
        else {
            throw DecodingError.dataCorruptedError(
                forKey: .w,
                in: container,
                debugDescription: "Surface point size needs at least one positive UInt32 axis"
            )
        }
    }

    enum CodingKeys: String, CodingKey { case w, h }
}

/// Live viewport metadata supplied by the Surface presenter out of band. It
/// is used only to derive a missing layout axis and is never snapshot state.
public struct SurfaceViewportSize: Equatable, Hashable, Sendable {
    public let w: Int
    public let h: Int

    public init(w: Int, h: Int) {
        self.w = w
        self.h = h
    }
}

public enum SurfaceBackground: Equatable, Hashable, Sendable {
    case transparent
    case solid(color: String)
}

extension SurfaceBackground: Codable {
    enum CodingKeys: String, CodingKey { case kind, color }
    enum Kind: String, Codable { case transparent, solid }

    public init(from decoder: Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        switch try container.decode(Kind.self, forKey: .kind) {
        case .transparent:
            self = .transparent
        case .solid:
            let color = try container.decode(String.self, forKey: .color)
            guard color.range(
                of: #"^#[0-9A-Fa-f]{6}([0-9A-Fa-f]{2})?$"#,
                options: .regularExpression
            ) != nil else {
                throw DecodingError.dataCorruptedError(
                    forKey: .color,
                    in: container,
                    debugDescription: "Surface solid color must be #RRGGBB or #RRGGBBAA sRGBA"
                )
            }
            self = .solid(color: color)
        }
    }

    public func encode(to encoder: Encoder) throws {
        var container = encoder.container(keyedBy: CodingKeys.self)
        switch self {
        case .transparent:
            try container.encode(Kind.transparent, forKey: .kind)
        case let .solid(color):
            try container.encode(Kind.solid, forKey: .kind)
            try container.encode(color, forKey: .color)
        }
    }
}

public enum SurfaceInputPolicy: String, Codable, Equatable, Hashable, Sendable {
    case none
    case pointer
    case pointerAndKeyboard
}

/// Reference-only Surface leaf. Scene commands and immutable resources stay
/// on USRF; composed frames never enter this value or `unpeel.ui`.
public struct SurfaceSpec: Codable, Equatable, Hashable, Sendable {
    public let reference: SurfaceReference
    public let cells: SurfaceCellSize?
    public let points: SurfacePointSize?
    public let background: SurfaceBackground
    public let inputPolicy: SurfaceInputPolicy

    public init(
        reference: SurfaceReference,
        cells: SurfaceCellSize? = nil,
        points: SurfacePointSize? = nil,
        background: SurfaceBackground = .transparent,
        inputPolicy: SurfaceInputPolicy = .none
    ) {
        self.reference = reference
        self.cells = cells
        self.points = points
        self.background = background
        self.inputPolicy = inputPolicy
    }

    enum CodingKeys: String, CodingKey {
        case reference, cells, points, background, inputPolicy
    }

    public init(from decoder: Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        reference = try container.decode(SurfaceReference.self, forKey: .reference)
        cells = try container.decodeIfPresent(SurfaceCellSize.self, forKey: .cells)
        points = try container.decodeIfPresent(SurfacePointSize.self, forKey: .points)
        background = try container.decodeIfPresent(
            SurfaceBackground.self,
            forKey: .background
        ) ?? .transparent
        inputPolicy = try container.decodeIfPresent(
            SurfaceInputPolicy.self,
            forKey: .inputPolicy
        ) ?? .none
        try Self.validateIdentifier(reference.sessionID, key: .reference, in: container)
        try Self.validateIdentifier(reference.streamID, key: .reference, in: container)
    }

    public func resolvedPointSize(viewport: SurfaceViewportSize) -> (w: Int, h: Int)? {
        guard let points else { return nil }
        switch (points.w, points.h) {
        case let (w?, h?):
            return (w, h)
        case let (w?, nil):
            return (w, ratioCeil(w, viewport.h, viewport.w))
        case let (nil, h?):
            return (ratioCeil(h, viewport.w, viewport.h), h)
        case (nil, nil):
            return nil
        }
    }

    private static func validateIdentifier<Key: CodingKey>(
        _ value: String,
        key: Key,
        in container: KeyedDecodingContainer<Key>
    ) throws {
        let punctuation = Set("._:/-".utf8)
        guard !value.isEmpty, value.utf8.count <= 256,
              value.utf8.allSatisfy({
                  (48...57).contains($0)
                      || (65...90).contains($0)
                      || (97...122).contains($0)
                      || punctuation.contains($0)
              })
        else {
            throw DecodingError.dataCorruptedError(
                forKey: key,
                in: container,
                debugDescription: "Surface reference must use portable identifiers"
            )
        }
    }
}

/// Closed visual intent for semantic buttons. Renderers map these roles to
/// their native control treatments rather than accepting arbitrary styling.
public enum UIButtonRole: String, Codable, Equatable, Hashable, Sendable {
    case standard = "default"
    case primary
    case destructive
}

public struct UIButtonSpec: Codable, Equatable, Hashable, Identifiable, Sendable {
    public let id: String
    public let label: String
    public let action: String
    public let role: UIButtonRole

    public init(
        id: String,
        label: String,
        action: String,
        role: UIButtonRole = .standard
    ) {
        self.id = id
        self.label = label
        self.action = action
        self.role = role
    }

    enum CodingKeys: String, CodingKey { case id, label, action, role }

    public init(from decoder: Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        id = try container.decode(String.self, forKey: .id)
        label = try container.decode(String.self, forKey: .label)
        action = try container.decode(String.self, forKey: .action)
        role = try container.decodeIfPresent(UIButtonRole.self, forKey: .role) ?? .standard
    }
}

/// Closed visual intent for one screen-level action.
public enum UIFooterActionRole: String, Codable, Equatable, Hashable, Sendable {
    case standard = "default"
    case danger
}

/// One App-owned action in an ordered screen footer.
public struct UIFooterActionSpec: Codable, Equatable, Hashable, Identifiable, Sendable {
    public let id: String
    public let label: String
    public let action: String
    public let accelerator: String?
    public let role: UIFooterActionRole
    public let disabled: Bool
    /// The action is in progress; renderers show an activity indicator.
    public let busy: Bool

    public init(
        id: String,
        label: String,
        action: String,
        accelerator: String? = nil,
        role: UIFooterActionRole = .standard,
        disabled: Bool = false,
        busy: Bool = false
    ) {
        self.id = id
        self.label = label
        self.action = action
        self.accelerator = accelerator
        self.role = role
        self.disabled = disabled
        self.busy = busy
    }

    enum CodingKeys: String, CodingKey {
        case id, label, action, accelerator, role, disabled, busy
    }

    public init(from decoder: Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        id = try container.decode(String.self, forKey: .id)
        label = try container.decode(String.self, forKey: .label)
        action = try container.decode(String.self, forKey: .action)
        accelerator = try container.decodeIfPresent(String.self, forKey: .accelerator)
        role = try container.decodeIfPresent(UIFooterActionRole.self, forKey: .role) ?? .standard
        disabled = try container.decodeIfPresent(Bool.self, forKey: .disabled) ?? false
        busy = try container.decodeIfPresent(Bool.self, forKey: .busy) ?? false
    }
}

/// Ordered action slot consumed identically by all screen-root renderers.
public struct UIFooterActionsSpec: Codable, Equatable, Hashable, Sendable {
    public var actions: [UIFooterActionSpec]

    public init(actions: [UIFooterActionSpec] = []) {
        self.actions = actions
    }

    public var isValid: Bool {
        guard actions.count <= 100_000 else { return false }
        var ids = Set<String>()
        var accelerators = Set<String>()
        for action in actions {
            guard isPortableUIIdentifier(action.id),
                  isPortableUIIdentifier(action.action),
                  action.label.utf8.count <= 4 * 1024,
                  !action.label.contains("\n"), !action.label.contains("\r"),
                  ids.insert(action.id).inserted
            else { return false }
            if let accelerator = action.accelerator {
                guard isFooterAccelerator(accelerator),
                      accelerators.insert(accelerator).inserted
                else { return false }
            }
        }
        return true
    }
}

private func isPortableUIIdentifier(_ value: String) -> Bool {
    let punctuation = Set("._:/-".utf8)
    return !value.isEmpty && value.utf8.count <= 256 && value.utf8.allSatisfy {
        (48...57).contains($0)
            || (65...90).contains($0)
            || (97...122).contains($0)
            || punctuation.contains($0)
    }
}

private func isFooterAccelerator(_ value: String) -> Bool {
    if ["escape", "enter", "space"].contains(value) { return true }
    if value.hasPrefix("ctrl+") {
        let key = value.dropFirst(5)
        return key.utf8.count == 1 && key.utf8.first.map {
            (48...57).contains($0) || (65...90).contains($0) || (97...122).contains($0)
        } == true
    }
    return value.utf8.count == 1 && value.utf8.first.map { (33...126).contains($0) } == true
}

/// One fixed Surface slot inside a CanvasPage. Unknown fields remain ignored
/// for protocol evolution, while the slot itself cannot contain another kind.
public struct UICanvasSurfaceSpec: Codable, Equatable, Hashable, Sendable {
    public let id: String
    public let surface: SurfaceSpec

    public init(id: String, surface: SurfaceSpec) {
        self.id = id
        self.surface = surface
    }

    enum CodingKeys: String, CodingKey { case id }

    public init(from decoder: Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        id = try container.decode(String.self, forKey: .id)
        surface = try SurfaceSpec(from: decoder)
    }

    public func encode(to encoder: Encoder) throws {
        var container = encoder.container(keyedBy: CodingKeys.self)
        try container.encode(id, forKey: .id)
        try surface.encode(to: encoder)
    }
}

public enum UICanvasControl: Equatable, Hashable, Sendable {
    case button(UIButtonSpec)
    case unsupported(kind: String)

    public var kind: String {
        switch self {
        case .button: "button"
        case let .unsupported(kind): kind
        }
    }
}

extension UICanvasControl: Codable {
    enum CodingKeys: String, CodingKey { case type }

    public init(from decoder: Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        let kind = try container.decode(String.self, forKey: .type)
        switch kind {
        case "button": self = .button(try UIButtonSpec(from: decoder))
        default: self = .unsupported(kind: kind)
        }
    }

    public func encode(to encoder: Encoder) throws {
        var container = encoder.container(keyedBy: CodingKeys.self)
        switch self {
        case let .button(button):
            try container.encode("button", forKey: .type)
            try button.encode(to: encoder)
        case let .unsupported(kind):
            try container.encode(kind, forKey: .type)
        }
    }
}

/// One canvas-sized Surface with a bounded, fixed top overlay of semantic
/// controls. The Surface stream remains out of band; controls stay on UI.
public struct CanvasPageSpec: Codable, Equatable, Hashable, Sendable {
    public let title: String
    public let surface: UICanvasSurfaceSpec
    public let controls: [UICanvasControl]

    public init(
        title: String,
        surface: UICanvasSurfaceSpec,
        controls: [UICanvasControl] = []
    ) {
        self.title = title
        self.surface = surface
        self.controls = controls
    }

    public var requiredCapabilities: [String]? {
        guard controls.allSatisfy({ if case .button = $0 { true } else { false } }) else {
            return nil
        }
        var capabilities = [
            UnpeelUIProtocol.canvasPageCapability,
            UnpeelUIProtocol.surfaceCapability,
        ]
        if !controls.isEmpty { capabilities.append(UnpeelUIProtocol.buttonCapability) }
        return capabilities
    }
}

private func ratioCeil(_ value: Int, _ numerator: Int, _ denominator: Int) -> Int {
    let scaled = UInt64(max(value, 0)) * UInt64(max(numerator, 1))
    let divisor = UInt64(max(denominator, 1))
    let resolved = scaled / divisor + (scaled % divisor == 0 ? 0 : 1)
    return Int(min(resolved, UInt64(UInt32.max)))
}

public enum UIToggleRole: String, Codable, Equatable, Hashable, Sendable {
    /// Marks the row done; shown as a checkbox.
    case completion
    /// A preference switch; shown as a switch and never strikes the row.
    case setting
}

public struct UIToggleSpec: Codable, Equatable, Hashable, Sendable {
    public let id: String
    public let label: String
    public let value: Bool
    public let setValue: String
    public let role: UIToggleRole

    public init(
        id: String,
        label: String,
        value: Bool,
        setValue: String,
        role: UIToggleRole = .completion
    ) {
        self.id = id
        self.label = label
        self.value = value
        self.setValue = setValue
        self.role = role
    }

    enum CodingKeys: String, CodingKey { case id, label, value, setValue, role }

    public init(from decoder: Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        id = try container.decode(String.self, forKey: .id)
        label = try container.decode(String.self, forKey: .label)
        value = try container.decode(Bool.self, forKey: .value)
        setValue = try container.decode(String.self, forKey: .setValue)
        role = try container.decodeIfPresent(UIToggleRole.self, forKey: .role) ?? .completion
    }

    public func encode(to encoder: Encoder) throws {
        var container = encoder.container(keyedBy: CodingKeys.self)
        try container.encode(id, forKey: .id)
        try container.encode(label, forKey: .label)
        try container.encode(value, forKey: .value)
        try container.encode(setValue, forKey: .setValue)
        if role != .completion { try container.encode(role, forKey: .role) }
    }
}

public struct UICheckmarkSpec: Codable, Equatable, Hashable, Sendable {
    public let id: String
    public let label: String
    public let value: Bool
    public let setValue: String

    public init(id: String, label: String, value: Bool, setValue: String) {
        self.id = id
        self.label = label
        self.value = value
        self.setValue = setValue
    }
}

public enum UIListItemTone: String, Codable, Equatable, Hashable, Sendable {
    case `default`
    case muted
    case accent
    case info
    case success
    case warning
    case danger
}

public enum UIListItemEmphasis: String, Codable, Equatable, Hashable, Sendable {
    case regular
    case strong
}

public struct UIListItemTextRun: Codable, Equatable, Hashable, Sendable {
    public let text: String
    public let tone: UIListItemTone?
    public let emphasis: UIListItemEmphasis?

    public init(
        text: String,
        tone: UIListItemTone? = nil,
        emphasis: UIListItemEmphasis? = nil
    ) {
        self.text = text
        self.tone = tone
        self.emphasis = emphasis
    }
}

private func listItemRunsAreValid(
    _ runs: [UIListItemTextRun],
    fallback: String?
) -> Bool {
    if runs.isEmpty { return true }
    guard runs.count <= 256, let fallback else { return false }
    return runs.map(\.text).joined() == fallback && runs.allSatisfy {
        !$0.text.isEmpty && $0.text.utf8.count <= 16_384
            && !$0.text.contains("\n") && !$0.text.contains("\r")
            && !$0.text.contains("\0")
    }
}

public enum UIListItemActionRole: String, Codable, Equatable, Hashable, Sendable {
    case `default`
    case destructive
}

public enum UIListItemPrimaryRole: Equatable, Sendable {
    case `static`
    case toggle
    case checkmark
    case disclosure
    case command
    case destructive
}

public enum UIListPageBehavior: String, Codable, Equatable, Hashable, Sendable {
    case selection
    case scroll
}

public struct UIStatusSymbolSpec: Codable, Equatable, Hashable, Sendable {
    public let symbol: String
    public let label: String
    public let tone: UIListItemTone
    public let emphasis: UIListItemEmphasis
    public let preserveToneWhenSelected: Bool

    public init(
        symbol: String,
        label: String,
        tone: UIListItemTone = .default,
        emphasis: UIListItemEmphasis = .regular,
        preserveToneWhenSelected: Bool = false
    ) {
        self.symbol = symbol
        self.label = label
        self.tone = tone
        self.emphasis = emphasis
        self.preserveToneWhenSelected = preserveToneWhenSelected
    }

    enum CodingKeys: String, CodingKey {
        case symbol, label, tone, emphasis, preserveToneWhenSelected
    }

    public init(from decoder: Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        symbol = try container.decode(String.self, forKey: .symbol)
        label = try container.decode(String.self, forKey: .label)
        guard !symbol.isEmpty, !symbol.contains("\n"), !symbol.contains("\r") else {
            throw DecodingError.dataCorruptedError(
                forKey: .symbol,
                in: container,
                debugDescription: "Status symbol must be a non-empty single line"
            )
        }
        tone = try container.decodeIfPresent(UIListItemTone.self, forKey: .tone) ?? .default
        emphasis = try container.decodeIfPresent(UIListItemEmphasis.self, forKey: .emphasis) ?? .regular
        preserveToneWhenSelected = try container.decodeIfPresent(
            Bool.self,
            forKey: .preserveToneWhenSelected
        ) ?? false
    }
}

public struct UIBadgeSpec: Codable, Equatable, Hashable, Sendable {
    public let text: String
    public let tone: UIListItemTone

    public init(text: String, tone: UIListItemTone = .muted) {
        self.text = text
        self.tone = tone
    }

    enum CodingKeys: String, CodingKey { case text, tone }

    public init(from decoder: Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        text = try container.decode(String.self, forKey: .text)
        guard !text.contains("\n"), !text.contains("\r") else {
            throw DecodingError.dataCorruptedError(
                forKey: .text,
                in: container,
                debugDescription: "Badge text must be a single line"
            )
        }
        tone = try container.decodeIfPresent(UIListItemTone.self, forKey: .tone) ?? .default
    }
}

/// Closed, read-only numeric history shared by terminal, Swift Charts, and web.
public struct UISparklineSpec: Codable, Equatable, Hashable, Sendable {
    public let id: String
    public let series: [Double]
    public let min: Double?
    public let max: Double?
    public let caption: String?
    public let unit: String?
    public let accessibilityText: String
    public let activate: String?

    public init(
        id: String,
        series: [Double],
        min: Double? = nil,
        max: Double? = nil,
        caption: String? = nil,
        unit: String? = nil,
        accessibilityText: String,
        activate: String? = nil
    ) {
        self.id = id
        self.series = series
        self.min = min
        self.max = max
        self.caption = caption
        self.unit = unit
        self.accessibilityText = accessibilityText
        self.activate = activate
    }

    enum CodingKeys: String, CodingKey {
        case id, series, min, max, caption, unit, accessibilityText, activate
    }

    public init(from decoder: Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        id = try container.decode(String.self, forKey: .id)
        series = try container.decode([Double].self, forKey: .series)
        min = try container.decodeIfPresent(Double.self, forKey: .min)
        max = try container.decodeIfPresent(Double.self, forKey: .max)
        caption = try container.decodeIfPresent(String.self, forKey: .caption)
        unit = try container.decodeIfPresent(String.self, forKey: .unit)
        accessibilityText = try container.decode(String.self, forKey: .accessibilityText)
        activate = try container.decodeIfPresent(String.self, forKey: .activate)

        guard isValid else {
            throw DecodingError.dataCorruptedError(
                forKey: .series,
                in: container,
                debugDescription: "Sparkline needs finite data, containing bounds, and accessibility text"
            )
        }
    }

    var isValid: Bool {
        (1...100_000).contains(series.count) && series.allSatisfy(\.isFinite)
            && (min?.isFinite ?? true) && (max?.isFinite ?? true)
            && (min.map({ lower in series.allSatisfy { $0 >= lower } }) ?? true)
            && (max.map({ upper in series.allSatisfy { $0 <= upper } }) ?? true)
            && (min == nil || max == nil || min! < max!)
            && chartAccessibilityIsValid(accessibilityText)
            && [caption, unit].compactMap({ $0 }).allSatisfy {
                chartSingleLineIsValid($0, allowEmpty: true)
            }
            && chartIdentifierIsValid(id)
            && (activate.map(chartIdentifierIsValid) ?? true)
    }

    /// The Rust-spec domain rule: inferred bounds include zero and an all-zero
    /// series expands to 0...1.
    public var resolvedBounds: ClosedRange<Double> {
        let seriesMinimum = series.min() ?? 0
        let seriesMaximum = series.max() ?? 0
        let lower = min ?? Swift.min(seriesMinimum, 0)
        var upper = max ?? Swift.max(seriesMaximum, 0)
        if lower == upper { upper = lower + 1 }
        return lower...upper
    }

    public var normalizedSeries: [Double] {
        let bounds = resolvedBounds
        let range = bounds.upperBound - bounds.lowerBound
        return series.map { value in
            Swift.min(Swift.max((value - bounds.lowerBound) / range, 0), 1)
        }
    }
}

public enum UIBarChartEmphasis: String, Codable, Equatable, Hashable, Sendable {
    case standard = "default"
    case accent
    case danger
}

public struct UIBarChartBar: Codable, Equatable, Hashable, Sendable {
    public let label: String
    public let value: Double
    public let valueCaption: String?
    public let emphasis: UIBarChartEmphasis

    public init(
        label: String,
        value: Double,
        valueCaption: String? = nil,
        emphasis: UIBarChartEmphasis = .standard
    ) {
        self.label = label
        self.value = value
        self.valueCaption = valueCaption
        self.emphasis = emphasis
    }

    enum CodingKeys: String, CodingKey {
        case label, value, valueCaption, emphasis
    }

    public init(from decoder: Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        label = try container.decode(String.self, forKey: .label)
        value = try container.decode(Double.self, forKey: .value)
        valueCaption = try container.decodeIfPresent(String.self, forKey: .valueCaption)
        emphasis = try container.decodeIfPresent(
            UIBarChartEmphasis.self,
            forKey: .emphasis
        ) ?? .standard
    }

    var isValid: Bool {
        chartSingleLineIsValid(label, allowEmpty: false)
            && value.isFinite && value >= 0
            && (valueCaption.map { chartSingleLineIsValid($0, allowEmpty: true) } ?? true)
    }
}

public struct UIBarChartSpec: Codable, Equatable, Hashable, Sendable {
    public let id: String
    public let bars: [UIBarChartBar]
    public let accessibilityText: String
    public let activate: String?

    public init(
        id: String,
        bars: [UIBarChartBar],
        accessibilityText: String,
        activate: String? = nil
    ) {
        self.id = id
        self.bars = bars
        self.accessibilityText = accessibilityText
        self.activate = activate
    }

    enum CodingKeys: String, CodingKey {
        case id, bars, accessibilityText, activate
    }

    public init(from decoder: Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        id = try container.decode(String.self, forKey: .id)
        bars = try container.decode([UIBarChartBar].self, forKey: .bars)
        accessibilityText = try container.decode(String.self, forKey: .accessibilityText)
        activate = try container.decodeIfPresent(String.self, forKey: .activate)
        guard isValid else {
            throw DecodingError.dataCorruptedError(
                forKey: .bars,
                in: container,
                debugDescription: "BarChart needs labeled non-negative bars and accessibility text"
            )
        }
    }

    var isValid: Bool {
        chartIdentifierIsValid(id)
            && (1...1_000).contains(bars.count)
            && bars.allSatisfy(\.isValid)
            && chartAccessibilityIsValid(accessibilityText)
            && (activate.map(chartIdentifierIsValid) ?? true)
    }

    public var normalizedValues: [Double] {
        let maximum = max(bars.map(\.value).max() ?? 0, 1)
        return bars.map { min(max($0.value / maximum, 0), 1) }
    }
}

public struct UILineChartPoint: Codable, Equatable, Hashable, Sendable {
    public let x: Double
    public let y: Double

    public init(x: Double, y: Double) {
        self.x = x
        self.y = y
    }
}

public struct UILineChartSeries: Codable, Equatable, Hashable, Sendable {
    public let name: String
    public let points: [UILineChartPoint]

    public init(name: String, points: [UILineChartPoint]) {
        self.name = name
        self.points = points
    }
}

public struct UILineChartBounds: Codable, Equatable, Hashable, Sendable {
    public let min: Double
    public let max: Double

    public init(min: Double, max: Double) {
        self.min = min
        self.max = max
    }

    var isValid: Bool {
        min.isFinite && max.isFinite && min < max
    }
}

public struct UILineChartAxis: Codable, Equatable, Hashable, Sendable {
    public let bounds: UILineChartBounds?
    public let label: String?

    public init(bounds: UILineChartBounds? = nil, label: String? = nil) {
        self.bounds = bounds
        self.label = label
    }
}

public struct UILineChartSpec: Codable, Equatable, Hashable, Sendable {
    public let id: String
    public let series: [UILineChartSeries]
    public let xAxis: UILineChartAxis
    public let yAxis: UILineChartAxis
    public let accessibilityText: String
    public let activate: String?

    public init(
        id: String,
        series: [UILineChartSeries],
        xAxis: UILineChartAxis = .init(),
        yAxis: UILineChartAxis = .init(),
        accessibilityText: String,
        activate: String? = nil
    ) {
        self.id = id
        self.series = series
        self.xAxis = xAxis
        self.yAxis = yAxis
        self.accessibilityText = accessibilityText
        self.activate = activate
    }

    enum CodingKeys: String, CodingKey {
        case id, series, xAxis, yAxis, accessibilityText, activate
    }

    public init(from decoder: Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        id = try container.decode(String.self, forKey: .id)
        series = try container.decode([UILineChartSeries].self, forKey: .series)
        xAxis = try container.decodeIfPresent(UILineChartAxis.self, forKey: .xAxis) ?? .init()
        yAxis = try container.decodeIfPresent(UILineChartAxis.self, forKey: .yAxis) ?? .init()
        accessibilityText = try container.decode(String.self, forKey: .accessibilityText)
        activate = try container.decodeIfPresent(String.self, forKey: .activate)
        guard isValid else {
            throw DecodingError.dataCorruptedError(
                forKey: .series,
                in: container,
                debugDescription: "LineChart needs finite named series, containing axes, and accessibility text"
            )
        }
    }

    var isValid: Bool {
        let names = Set(series.map(\.name))
        let points = series.flatMap(\.points)
        return chartIdentifierIsValid(id)
            && (1...16).contains(series.count)
            && names.count == series.count
            && series.allSatisfy {
                chartSingleLineIsValid($0.name, allowEmpty: false) && !$0.points.isEmpty
                    && $0.points.allSatisfy { $0.x.isFinite && $0.y.isFinite }
            }
            && points.count <= 100_000
            && axisIsValid(xAxis, values: points.map(\.x))
            && axisIsValid(yAxis, values: points.map(\.y))
            && chartAccessibilityIsValid(accessibilityText)
            && (activate.map(chartIdentifierIsValid) ?? true)
    }

    public var resolvedXBounds: ClosedRange<Double> {
        resolvedBounds(axis: xAxis, values: series.flatMap { $0.points.map(\.x) })
    }

    public var resolvedYBounds: ClosedRange<Double> {
        resolvedBounds(axis: yAxis, values: series.flatMap { $0.points.map(\.y) })
    }

    private func axisIsValid(_ axis: UILineChartAxis, values: [Double]) -> Bool {
        guard axis.label.map({ chartSingleLineIsValid($0, allowEmpty: true) }) ?? true,
              axis.bounds?.isValid ?? true
        else { return false }
        guard let bounds = axis.bounds else { return true }
        return values.allSatisfy { (bounds.min...bounds.max).contains($0) }
    }

    private func resolvedBounds(
        axis: UILineChartAxis,
        values: [Double]
    ) -> ClosedRange<Double> {
        if let bounds = axis.bounds { return bounds.min...bounds.max }
        let lower = values.min() ?? 0
        var upper = values.max() ?? 0
        if lower == upper { upper = lower + 1 }
        return lower...upper
    }
}

public struct UIGaugeSpec: Codable, Equatable, Hashable, Sendable {
    public let id: String
    public let ratio: Double
    public let label: String
    public let caption: String?
    public let accessibilityText: String
    public let activate: String?

    public init(
        id: String,
        ratio: Double,
        label: String,
        caption: String? = nil,
        accessibilityText: String,
        activate: String? = nil
    ) {
        self.id = id
        self.ratio = ratio
        self.label = label
        self.caption = caption
        self.accessibilityText = accessibilityText
        self.activate = activate
    }

    enum CodingKeys: String, CodingKey {
        case id, ratio, label, caption, accessibilityText, activate
    }

    public init(from decoder: Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        id = try container.decode(String.self, forKey: .id)
        ratio = try container.decode(Double.self, forKey: .ratio)
        label = try container.decode(String.self, forKey: .label)
        caption = try container.decodeIfPresent(String.self, forKey: .caption)
        accessibilityText = try container.decode(String.self, forKey: .accessibilityText)
        activate = try container.decodeIfPresent(String.self, forKey: .activate)
        guard isValid else {
            throw DecodingError.dataCorruptedError(
                forKey: .ratio,
                in: container,
                debugDescription: "Gauge ratio must be 0...1 with label and accessibility text"
            )
        }
    }

    var isValid: Bool {
        chartIdentifierIsValid(id)
            && ratio.isFinite && (0...1).contains(ratio)
            && chartSingleLineIsValid(label, allowEmpty: false)
            && (caption.map { chartSingleLineIsValid($0, allowEmpty: false) } ?? true)
            && chartAccessibilityIsValid(accessibilityText)
            && (activate.map(chartIdentifierIsValid) ?? true)
    }

    public var percentageLabel: String {
        "\(label)  \(valueLabel)"
    }

    public var percentageValueLabel: String {
        "\(Int((ratio * 100).rounded()))%"
    }

    public var valueLabel: String {
        caption ?? percentageValueLabel
    }
}

private func chartIdentifierIsValid(_ value: String) -> Bool {
    !value.isEmpty && value.utf8.count <= 256 && value.utf8.allSatisfy {
        ($0 >= 48 && $0 <= 57) || ($0 >= 65 && $0 <= 90) || ($0 >= 97 && $0 <= 122)
            || [46, 95, 58, 47, 45].contains($0)
    }
}

private func chartSingleLineIsValid(_ value: String, allowEmpty: Bool) -> Bool {
    value.utf8.count <= 4_096 && !value.contains("\n") && !value.contains("\r")
        && !value.contains("\0")
        && (allowEmpty || !value.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty)
}

private func chartAccessibilityIsValid(_ value: String) -> Bool {
    value.utf8.count <= 16_384
        && !value.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty
        && !value.contains("\r") && !value.contains("\0")
}

public enum UIListItemSlot: Equatable, Hashable, Sendable {
    case toggle(UIToggleSpec)
    case status(UIStatusSymbolSpec)
    case badge(UIBadgeSpec)
    case sparkline(UISparklineSpec)
    case gauge(UIGaugeSpec)
    case disclosure
    case checkmark(UICheckmarkSpec)
    case unsupported(kind: String)

    public var kind: String {
        switch self {
        case .toggle: "toggle"
        case .status: "status"
        case .badge: "badge"
        case .sparkline: "sparkline"
        case .gauge: "gauge"
        case .disclosure: "disclosure"
        case .checkmark: "checkmark"
        case let .unsupported(kind): kind
        }
    }
}

extension UIListItemSlot: Codable {
    enum CodingKeys: String, CodingKey { case type }

    public init(from decoder: Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        let kind = try container.decode(String.self, forKey: .type)
        switch kind {
        case "toggle": self = .toggle(try UIToggleSpec(from: decoder))
        case "status": self = .status(try UIStatusSymbolSpec(from: decoder))
        case "badge": self = .badge(try UIBadgeSpec(from: decoder))
        case "sparkline": self = .sparkline(try UISparklineSpec(from: decoder))
        case "gauge": self = .gauge(try UIGaugeSpec(from: decoder))
        case "disclosure": self = .disclosure
        case "checkmark": self = .checkmark(try UICheckmarkSpec(from: decoder))
        default: self = .unsupported(kind: kind)
        }
    }

    public func encode(to encoder: Encoder) throws {
        var container = encoder.container(keyedBy: CodingKeys.self)
        switch self {
        case let .toggle(toggle):
            try container.encode("toggle", forKey: .type)
            try toggle.encode(to: encoder)
        case let .status(status):
            try container.encode("status", forKey: .type)
            try status.encode(to: encoder)
        case let .badge(badge):
            try container.encode("badge", forKey: .type)
            try badge.encode(to: encoder)
        case let .sparkline(sparkline):
            try container.encode("sparkline", forKey: .type)
            try sparkline.encode(to: encoder)
        case let .gauge(gauge):
            try container.encode("gauge", forKey: .type)
            try gauge.encode(to: encoder)
        case .disclosure:
            try container.encode("disclosure", forKey: .type)
        case let .checkmark(checkmark):
            try container.encode("checkmark", forKey: .type)
            try checkmark.encode(to: encoder)
        case let .unsupported(kind):
            try container.encode(kind, forKey: .type)
        }
    }
}

/// How a List lays out each row. Native rows always stack the detail; the
/// value moves beneath the label for `.stacked`, and `.auto` lets the row
/// choose by its available width.
public enum UIListRowLayout: Equatable, Hashable, Sendable {
    case inline
    case stacked
    case auto(stackBelowWidth: Int)
}

extension UIListRowLayout: Codable {
    enum CodingKeys: String, CodingKey { case type, stackBelowWidth }

    public init(from decoder: Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        switch try container.decode(String.self, forKey: .type) {
        case "inline": self = .inline
        case "stacked": self = .stacked
        case "auto":
            self = .auto(stackBelowWidth: try container.decode(Int.self, forKey: .stackBelowWidth))
        case let other:
            throw DecodingError.dataCorruptedError(
                forKey: .type,
                in: container,
                debugDescription: "unsupported List rowLayout \(other)"
            )
        }
    }

    public func encode(to encoder: Encoder) throws {
        var container = encoder.container(keyedBy: CodingKeys.self)
        switch self {
        case .inline: try container.encode("inline", forKey: .type)
        case .stacked: try container.encode("stacked", forKey: .type)
        case let .auto(width):
            try container.encode("auto", forKey: .type)
            try container.encode(width, forKey: .stackBelowWidth)
        }
    }
}

/// A full-width band on its own line above or below a ListItem's text.
public enum UIListItemBand: Equatable, Hashable, Sendable {
    case gauge(UIGaugeSpec)
    case sparkline(UISparklineSpec)
    case text(String, tone: UIListItemTone)
    case divider
    case unsupported(kind: String)

    public var id: String? {
        switch self {
        case let .gauge(gauge): gauge.id
        case let .sparkline(sparkline): sparkline.id
        case .text, .divider, .unsupported: nil
        }
    }
}

extension UIListItemBand: Codable {
    enum CodingKeys: String, CodingKey { case type, text, tone }

    public init(from decoder: Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        let kind = try container.decode(String.self, forKey: .type)
        switch kind {
        case "gauge": self = .gauge(try UIGaugeSpec(from: decoder))
        case "sparkline": self = .sparkline(try UISparklineSpec(from: decoder))
        case "text":
            self = .text(
                try container.decode(String.self, forKey: .text),
                tone: try container.decodeIfPresent(UIListItemTone.self, forKey: .tone) ?? .default
            )
        case "divider": self = .divider
        default: self = .unsupported(kind: kind)
        }
    }

    public func encode(to encoder: Encoder) throws {
        var container = encoder.container(keyedBy: CodingKeys.self)
        switch self {
        case let .gauge(gauge):
            try container.encode("gauge", forKey: .type)
            try gauge.encode(to: encoder)
        case let .sparkline(sparkline):
            try container.encode("sparkline", forKey: .type)
            try sparkline.encode(to: encoder)
        case let .text(text, tone):
            try container.encode("text", forKey: .type)
            try container.encode(text, forKey: .text)
            if tone != .default { try container.encode(tone, forKey: .tone) }
        case .divider:
            try container.encode("divider", forKey: .type)
        case let .unsupported(kind):
            try container.encode(kind, forKey: .type)
        }
    }
}

public enum UIListItemMediaSide: String, Codable, Equatable, Hashable, Sendable {
    case leading
    case trailing
}

/// A media column spanning every line of a ListItem. `spec` is the real
/// image; without it renderers show a tone-colored block with the glyph.
public struct UIListItemMedia: Codable, Equatable, Hashable, Sendable {
    public let side: UIListItemMediaSide
    /// Terminal columns reserved for the block (1 through 12).
    public let width: Int
    public let glyph: String?
    public let tone: UIListItemTone
    public let spec: MediaSpec?

    public init(
        side: UIListItemMediaSide = .leading,
        width: Int,
        glyph: String? = nil,
        tone: UIListItemTone = .default,
        spec: MediaSpec? = nil
    ) {
        self.side = side
        self.width = width
        self.glyph = glyph
        self.tone = tone
        self.spec = spec
    }

    enum CodingKeys: String, CodingKey { case side, width, glyph, tone, spec }

    public init(from decoder: Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        side = try container.decodeIfPresent(UIListItemMediaSide.self, forKey: .side) ?? .leading
        width = try container.decode(Int.self, forKey: .width)
        glyph = try container.decodeIfPresent(String.self, forKey: .glyph)
        tone = try container.decodeIfPresent(UIListItemTone.self, forKey: .tone) ?? .default
        spec = try container.decodeIfPresent(MediaSpec.self, forKey: .spec)
        guard (1...12).contains(width) else {
            throw DecodingError.dataCorruptedError(
                forKey: .width,
                in: container,
                debugDescription: "ListItem media width must be between 1 and 12"
            )
        }
    }

    public func encode(to encoder: Encoder) throws {
        var container = encoder.container(keyedBy: CodingKeys.self)
        if side != .leading { try container.encode(side, forKey: .side) }
        try container.encode(width, forKey: .width)
        try container.encodeIfPresent(glyph, forKey: .glyph)
        if tone != .default { try container.encode(tone, forKey: .tone) }
        try container.encodeIfPresent(spec, forKey: .spec)
    }
}

public struct UIListItemSpec: Codable, Equatable, Hashable, Identifiable, Sendable {
    public let id: String
    public let label: String
    public let labelRuns: [UIListItemTextRun]
    public let labelTone: UIListItemTone
    public let emphasis: UIListItemEmphasis
    public let detail: String?
    public let detailRuns: [UIListItemTextRun]
    public let value: String?
    public let valueRuns: [UIListItemTextRun]
    public let valueTone: UIListItemTone
    public let valueMinWidth: Int?
    public var done: Bool
    public let busy: Bool
    public var leading: UIListItemSlot?
    public var trailing: UIListItemSlot?
    public var accessory: UIListItemSlot?
    /// Full-width band above the text rows.
    public let top: UIListItemBand?
    /// Full-width band below the text rows.
    public let bottom: UIListItemBand?
    /// Media column spanning the item.
    public let media: UIListItemMedia?
    /// A passive separator row; `label` is an optional caption.
    public let divider: Bool
    public let delete: String?
    public let activate: String?
    public let actionRole: UIListItemActionRole

    public init(
        id: String,
        label: String,
        labelRuns: [UIListItemTextRun] = [],
        labelTone: UIListItemTone = .default,
        emphasis: UIListItemEmphasis = .regular,
        detail: String? = nil,
        detailRuns: [UIListItemTextRun] = [],
        value: String? = nil,
        valueRuns: [UIListItemTextRun] = [],
        valueTone: UIListItemTone = .muted,
        valueMinWidth: Int? = nil,
        done: Bool = false,
        busy: Bool = false,
        leading: UIListItemSlot? = nil,
        trailing: UIListItemSlot? = nil,
        accessory: UIListItemSlot? = nil,
        top: UIListItemBand? = nil,
        bottom: UIListItemBand? = nil,
        media: UIListItemMedia? = nil,
        divider: Bool = false,
        delete: String? = nil,
        activate: String? = nil,
        actionRole: UIListItemActionRole = .default
    ) {
        self.id = id
        self.label = label
        self.labelRuns = labelRuns
        self.labelTone = labelTone
        self.emphasis = emphasis
        self.detail = detail
        self.detailRuns = detailRuns
        self.value = value
        self.valueRuns = valueRuns
        self.valueTone = valueTone
        self.valueMinWidth = valueMinWidth
        self.done = done
        self.busy = busy
        self.leading = leading
        self.trailing = trailing
        self.accessory = accessory
        self.top = top
        self.bottom = bottom
        self.media = media
        self.divider = divider
        self.delete = delete
        self.activate = activate
        self.actionRole = actionRole
    }

    enum CodingKeys: String, CodingKey {
        case id, label, labelRuns, labelTone, emphasis, detail, detailRuns
        case value, valueRuns, valueTone, valueMinWidth
        case done, busy, leading, trailing, accessory, top, bottom, media, divider
        case delete, activate, actionRole
    }

    public init(from decoder: Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        id = try container.decode(String.self, forKey: .id)
        label = try container.decode(String.self, forKey: .label)
        labelRuns = try container.decodeIfPresent(
            [UIListItemTextRun].self,
            forKey: .labelRuns
        ) ?? []
        labelTone = try container.decodeIfPresent(UIListItemTone.self, forKey: .labelTone) ?? .default
        emphasis = try container.decodeIfPresent(UIListItemEmphasis.self, forKey: .emphasis) ?? .regular
        detail = try container.decodeIfPresent(String.self, forKey: .detail)
        detailRuns = try container.decodeIfPresent(
            [UIListItemTextRun].self,
            forKey: .detailRuns
        ) ?? []
        value = try container.decodeIfPresent(String.self, forKey: .value)
        valueRuns = try container.decodeIfPresent(
            [UIListItemTextRun].self,
            forKey: .valueRuns
        ) ?? []
        valueTone = try container.decodeIfPresent(UIListItemTone.self, forKey: .valueTone) ?? .muted
        valueMinWidth = try container.decodeIfPresent(Int.self, forKey: .valueMinWidth)
        done = try container.decodeIfPresent(Bool.self, forKey: .done) ?? false
        busy = try container.decodeIfPresent(Bool.self, forKey: .busy) ?? false
        leading = try container.decodeIfPresent(UIListItemSlot.self, forKey: .leading)
        trailing = try container.decodeIfPresent(UIListItemSlot.self, forKey: .trailing)
        accessory = try container.decodeIfPresent(UIListItemSlot.self, forKey: .accessory)
        top = try container.decodeIfPresent(UIListItemBand.self, forKey: .top)
        bottom = try container.decodeIfPresent(UIListItemBand.self, forKey: .bottom)
        media = try container.decodeIfPresent(UIListItemMedia.self, forKey: .media)
        divider = try container.decodeIfPresent(Bool.self, forKey: .divider) ?? false
        delete = try container.decodeIfPresent(String.self, forKey: .delete)
        activate = try container.decodeIfPresent(String.self, forKey: .activate)
        actionRole = try container.decodeIfPresent(
            UIListItemActionRole.self,
            forKey: .actionRole
        ) ?? .default
        guard [label, detail, value].compactMap({ $0 }).allSatisfy({
            !$0.contains("\n") && !$0.contains("\r")
        }), listItemRunsAreValid(labelRuns, fallback: label),
        listItemRunsAreValid(detailRuns, fallback: detail),
        listItemRunsAreValid(valueRuns, fallback: value) else {
            throw DecodingError.dataCorruptedError(
                forKey: .label,
                in: container,
                debugDescription: "ListItem label, detail, and value must be single-line"
            )
        }
        if let valueMinWidth, !(0...Int(UInt16.max)).contains(valueMinWidth) {
            throw DecodingError.dataCorruptedError(
                forKey: .valueMinWidth,
                in: container,
                debugDescription: "ListItem valueMinWidth must fit in UInt16"
            )
        }
        let toggles = [leading, trailing, accessory].compactMap { slot -> UIToggleSpec? in
            guard case let .toggle(toggle) = slot else { return nil }
            return toggle
        }
        let completion = toggles.first(where: { $0.role == .completion })
        guard toggles.count <= 1, completion?.value ?? done == done else {
            throw DecodingError.dataCorruptedError(
                forKey: .done,
                in: container,
                debugDescription: "ListItem accepts one completion Toggle whose value matches done"
            )
        }
        let checkmarks = [leading, trailing, accessory].compactMap { slot -> UICheckmarkSpec? in
            guard case let .checkmark(checkmark) = slot else { return nil }
            return checkmark
        }
        let disclosures = [leading, trailing, accessory].filter { slot in
            guard case .disclosure = slot else { return false }
            return true
        }
        let sparklines = [leading, trailing, accessory].compactMap { slot -> UISparklineSpec? in
            guard case let .sparkline(sparkline) = slot else { return nil }
            return sparkline
        }
        let gauges = [leading, trailing, accessory].compactMap { slot -> UIGaugeSpec? in
            guard case let .gauge(gauge) = slot else { return nil }
            return gauge
        }
        guard checkmarks.count <= 1, disclosures.count <= 1 else {
            throw DecodingError.dataCorruptedError(
                forKey: .accessory,
                in: container,
                debugDescription: "ListItem accepts one checkmark or disclosure accessory"
            )
        }
        if !checkmarks.isEmpty {
            guard case .checkmark? = accessory else {
                throw DecodingError.dataCorruptedError(
                    forKey: .accessory,
                    in: container,
                    debugDescription: "Checkmark is accepted only in the accessory slot"
                )
            }
        }
        if !disclosures.isEmpty {
            guard case .disclosure? = accessory, activate != nil else {
                throw DecodingError.dataCorruptedError(
                    forKey: .accessory,
                    in: container,
                    debugDescription: "Disclosure accessory requires activate"
                )
            }
        }
        if !sparklines.isEmpty {
            guard sparklines.count == 1, case .sparkline? = trailing else {
                throw DecodingError.dataCorruptedError(
                    forKey: .trailing,
                    in: container,
                    debugDescription: "Sparkline is accepted only once in the trailing slot"
                )
            }
        }
        if !gauges.isEmpty {
            guard gauges.count == 1, case .gauge? = trailing else {
                throw DecodingError.dataCorruptedError(
                    forKey: .trailing,
                    in: container,
                    debugDescription: "Gauge is accepted only once in the trailing slot"
                )
            }
        }
        let independentRoles = (toggles.isEmpty ? 0 : 1)
            + (checkmarks.isEmpty ? 0 : 1)
            + (disclosures.isEmpty ? 0 : 1)
            + (sparklines.contains(where: { $0.activate != nil }) ? 1 : 0)
            + (gauges.contains(where: { $0.activate != nil }) ? 1 : 0)
            + (activate != nil && disclosures.isEmpty ? 1 : 0)
        guard independentRoles <= 1 else {
            throw DecodingError.dataCorruptedError(
                forKey: .activate,
                in: container,
                debugDescription: "ListItem primary role is ambiguous"
            )
        }
        guard actionRole != .destructive || activate != nil && disclosures.isEmpty else {
            throw DecodingError.dataCorruptedError(
                forKey: .actionRole,
                in: container,
                debugDescription: "destructive is accepted only for a plain command row"
            )
        }
    }

    public var primaryRole: UIListItemPrimaryRole {
        if divider { return .static }
        if [leading, trailing, accessory].contains(where: {
            guard case .toggle? = $0 else { return false }
            return true
        }) { return .toggle }
        if [leading, trailing, accessory].contains(where: {
            guard case .checkmark? = $0 else { return false }
            return true
        }) { return .checkmark }
        if [leading, trailing, accessory].contains(where: {
            guard case .disclosure? = $0 else { return false }
            return true
        }) { return .disclosure }
        if primarySparkline?.activate != nil { return .command }
        if primaryGauge?.activate != nil { return .command }
        if activate != nil {
            return actionRole == .destructive ? .destructive : .command
        }
        return .static
    }

    public var primaryToggle: UIToggleSpec? {
        for slot in [leading, trailing, accessory] {
            if case let .toggle(toggle)? = slot { return toggle }
        }
        return nil
    }

    public var primaryCheckmark: UICheckmarkSpec? {
        for slot in [leading, trailing, accessory] {
            if case let .checkmark(checkmark)? = slot { return checkmark }
        }
        return nil
    }

    public var primarySparkline: UISparklineSpec? {
        for slot in [leading, trailing, accessory] {
            if case let .sparkline(sparkline)? = slot { return sparkline }
        }
        return nil
    }

    public var primaryGauge: UIGaugeSpec? {
        for slot in [leading, trailing, accessory] {
            if case let .gauge(gauge)? = slot { return gauge }
        }
        return nil
    }
}

public struct UIListSpec: Codable, Equatable, Hashable, Sendable {
    public let id: String
    public var items: [UIListItemSpec]
    public let emptyMessage: String
    public var selectedID: String?
    public let select: String?
    public let scrollPadding: Int
    public let pageOverlap: Int
    public let pageBehavior: UIListPageBehavior
    public let spacePagesDown: Bool
    public let rowLayout: UIListRowLayout
    public let contextMenu: UIMenuSpec?

    public init(
        id: String,
        items: [UIListItemSpec],
        emptyMessage: String = "",
        selectedID: String? = nil,
        select: String? = nil,
        scrollPadding: Int = 0,
        pageOverlap: Int = 1,
        pageBehavior: UIListPageBehavior = .selection,
        spacePagesDown: Bool = false,
        rowLayout: UIListRowLayout = .inline,
        contextMenu: UIMenuSpec? = nil
    ) {
        self.id = id
        self.items = items
        self.emptyMessage = emptyMessage
        self.selectedID = selectedID
        self.select = select
        self.scrollPadding = scrollPadding
        self.pageOverlap = pageOverlap
        self.pageBehavior = pageBehavior
        self.spacePagesDown = spacePagesDown
        self.rowLayout = rowLayout
        self.contextMenu = contextMenu
    }

    enum CodingKeys: String, CodingKey {
        case id, items, emptyMessage, selectedID = "selectedId", select, scrollPadding
        case pageOverlap, pageBehavior, spacePagesDown, rowLayout, contextMenu
    }

    public init(from decoder: Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        id = try container.decode(String.self, forKey: .id)
        items = try container.decode([UIListItemSpec].self, forKey: .items)
        emptyMessage = try container.decodeIfPresent(String.self, forKey: .emptyMessage) ?? ""
        selectedID = try container.decodeIfPresent(String.self, forKey: .selectedID)
        select = try container.decodeIfPresent(String.self, forKey: .select)
        scrollPadding = try container.decodeIfPresent(Int.self, forKey: .scrollPadding) ?? 0
        pageOverlap = try container.decodeIfPresent(Int.self, forKey: .pageOverlap) ?? 1
        pageBehavior = try container.decodeIfPresent(UIListPageBehavior.self, forKey: .pageBehavior) ?? .selection
        spacePagesDown = try container.decodeIfPresent(Bool.self, forKey: .spacePagesDown) ?? false
        rowLayout = try container.decodeIfPresent(UIListRowLayout.self, forKey: .rowLayout) ?? .inline
        contextMenu = try container.decodeIfPresent(UIMenuSpec.self, forKey: .contextMenu)
        guard (0...Int(UInt16.max)).contains(scrollPadding),
              (0...Int(UInt16.max)).contains(pageOverlap)
        else {
            throw DecodingError.dataCorruptedError(
                forKey: .scrollPadding,
                in: container,
                debugDescription: "List scrollPadding and pageOverlap must fit in UInt16"
            )
        }
        if let selectedID, !items.contains(where: { $0.id == selectedID }) {
            throw DecodingError.dataCorruptedError(
                forKey: .selectedID,
                in: container,
                debugDescription: "List selectedId must identify one of its items"
            )
        }
    }
}

public enum UIContentFont: String, Codable, Equatable, Hashable, Sendable {
    case body
    case monospace
}

public enum UIContentTone: String, Codable, Equatable, Hashable, Sendable {
    case `default`
    case muted
    case accent
    case info
    case success
    case warning
    case danger
}

public enum UIContentEmphasis: String, Codable, Equatable, Hashable, Sendable {
    case regular
    case strong
    case italic
}

public enum UIContentLineTone: String, Codable, Equatable, Hashable, Sendable {
    case `default`
    case muted
    case header
    case added
    case removed
}

public struct UIContentRun: Codable, Equatable, Hashable, Sendable {
    public let text: String
    public let tone: UIContentTone
    public let emphasis: UIContentEmphasis

    public init(
        text: String,
        tone: UIContentTone = .default,
        emphasis: UIContentEmphasis = .regular
    ) {
        self.text = text
        self.tone = tone
        self.emphasis = emphasis
    }

    enum CodingKeys: String, CodingKey { case text, tone, emphasis }

    public init(from decoder: Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        text = try container.decode(String.self, forKey: .text)
        tone = try container.decodeIfPresent(UIContentTone.self, forKey: .tone) ?? .default
        emphasis = try container.decodeIfPresent(
            UIContentEmphasis.self,
            forKey: .emphasis
        ) ?? .regular
        guard !text.contains("\n"), !text.contains("\r"), !text.contains("\0") else {
            throw DecodingError.dataCorruptedError(
                forKey: .text,
                in: container,
                debugDescription: "Content runs must stay within one logical line"
            )
        }
    }
}

public struct UIContentLine: Codable, Equatable, Hashable, Identifiable, Sendable {
    public let id: String
    public let runs: [UIContentRun]
    public let tone: UIContentLineTone

    public init(
        id: String,
        runs: [UIContentRun],
        tone: UIContentLineTone = .default
    ) {
        self.id = id
        self.runs = runs
        self.tone = tone
    }

    enum CodingKeys: String, CodingKey { case id, runs, tone }

    public init(from decoder: Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        id = try container.decode(String.self, forKey: .id)
        runs = try container.decode([UIContentRun].self, forKey: .runs)
        tone = try container.decodeIfPresent(UIContentLineTone.self, forKey: .tone) ?? .default
    }
}

public struct UIContentSelection: Codable, Equatable, Hashable, Sendable {
    public let anchorID: String
    public let headID: String

    public init(anchorID: String, headID: String) {
        self.anchorID = anchorID
        self.headID = headID
    }

    enum CodingKeys: String, CodingKey { case anchorID = "anchorId", headID = "headId" }
}

public struct UIContentSpec: Codable, Equatable, Hashable, Sendable {
    public let id: String
    public let label: String
    public var lines: [UIContentLine]
    public let wrap: Bool
    public let font: UIContentFont
    public let emptyMessage: String
    public var selection: UIContentSelection?
    public let select: String?
    public let contextMenu: UIMenuSpec?

    public init(
        id: String,
        label: String,
        lines: [UIContentLine],
        wrap: Bool = true,
        font: UIContentFont = .body,
        emptyMessage: String = "",
        selection: UIContentSelection? = nil,
        select: String? = nil,
        contextMenu: UIMenuSpec? = nil
    ) {
        self.id = id
        self.label = label
        self.lines = lines
        self.wrap = wrap
        self.font = font
        self.emptyMessage = emptyMessage
        self.selection = selection
        self.select = select
        self.contextMenu = contextMenu
    }

    enum CodingKeys: String, CodingKey {
        case id, label, lines, wrap, font, emptyMessage, selection, select, contextMenu
    }

    public init(from decoder: Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        id = try container.decode(String.self, forKey: .id)
        label = try container.decode(String.self, forKey: .label)
        lines = try container.decodeIfPresent([UIContentLine].self, forKey: .lines) ?? []
        wrap = try container.decodeIfPresent(Bool.self, forKey: .wrap) ?? true
        font = try container.decodeIfPresent(UIContentFont.self, forKey: .font) ?? .body
        emptyMessage = try container.decodeIfPresent(String.self, forKey: .emptyMessage) ?? ""
        selection = try container.decodeIfPresent(UIContentSelection.self, forKey: .selection)
        select = try container.decodeIfPresent(String.self, forKey: .select)
        contextMenu = try container.decodeIfPresent(UIMenuSpec.self, forKey: .contextMenu)
        let ids = Set(lines.map(\.id))
        guard ids.count == lines.count,
              selection.map({ ids.contains($0.anchorID) && ids.contains($0.headID) }) ?? true,
              selection == nil || select != nil
        else {
            throw DecodingError.dataCorruptedError(
                forKey: .lines,
                in: container,
                debugDescription: "Content line ids and selection must be valid"
            )
        }
    }
}

public struct UIInputSpec: Codable, Equatable, Hashable, Sendable {
    public let id: String
    public let label: String
    public var value: String
    public let placeholder: String
    public let setValue: String?
    public let submit: String?

    public init(
        id: String,
        label: String,
        value: String = "",
        placeholder: String = "",
        setValue: String? = nil,
        submit: String? = nil
    ) {
        self.id = id
        self.label = label
        self.value = value
        self.placeholder = placeholder
        self.setValue = setValue
        self.submit = submit
    }

    enum CodingKeys: String, CodingKey { case id, label, value, placeholder, setValue, submit }

    public init(from decoder: Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        id = try container.decode(String.self, forKey: .id)
        label = try container.decode(String.self, forKey: .label)
        value = try container.decodeIfPresent(String.self, forKey: .value) ?? ""
        placeholder = try container.decodeIfPresent(String.self, forKey: .placeholder) ?? ""
        setValue = try container.decodeIfPresent(String.self, forKey: .setValue)
        submit = try container.decodeIfPresent(String.self, forKey: .submit)
    }
}

public enum UIPageHeaderSlot: Equatable, Hashable, Sendable {
    case input(UIInputSpec)
    case unsupported(kind: String)
}

extension UIPageHeaderSlot: Codable {
    enum CodingKeys: String, CodingKey { case type }

    public init(from decoder: Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        let kind = try container.decode(String.self, forKey: .type)
        switch kind {
        case "input": self = .input(try UIInputSpec(from: decoder))
        default: self = .unsupported(kind: kind)
        }
    }

    public func encode(to encoder: Encoder) throws {
        var container = encoder.container(keyedBy: CodingKeys.self)
        switch self {
        case let .input(input):
            try container.encode("input", forKey: .type)
            try input.encode(to: encoder)
        case let .unsupported(kind):
            try container.encode(kind, forKey: .type)
        }
    }
}

public enum UIPageBodySlot: Equatable, Hashable, Sendable {
    case list(UIListSpec)
    case content(UIContentSpec)
    case sparkline(UISparklineSpec)
    case barChart(UIBarChartSpec)
    case lineChart(UILineChartSpec)
    case gauge(UIGaugeSpec)
    case unsupported(kind: String)
}

extension UIPageBodySlot: Codable {
    enum CodingKeys: String, CodingKey { case type }

    public init(from decoder: Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        let kind = try container.decode(String.self, forKey: .type)
        switch kind {
        case "list": self = .list(try UIListSpec(from: decoder))
        case "content": self = .content(try UIContentSpec(from: decoder))
        case "sparkline": self = .sparkline(try UISparklineSpec(from: decoder))
        case "barChart": self = .barChart(try UIBarChartSpec(from: decoder))
        case "lineChart": self = .lineChart(try UILineChartSpec(from: decoder))
        case "gauge": self = .gauge(try UIGaugeSpec(from: decoder))
        default: self = .unsupported(kind: kind)
        }
    }

    public func encode(to encoder: Encoder) throws {
        var container = encoder.container(keyedBy: CodingKeys.self)
        switch self {
        case let .list(list):
            try container.encode("list", forKey: .type)
            try list.encode(to: encoder)
        case let .content(content):
            try container.encode("content", forKey: .type)
            try content.encode(to: encoder)
        case let .sparkline(sparkline):
            try container.encode("sparkline", forKey: .type)
            try sparkline.encode(to: encoder)
        case let .barChart(chart):
            try container.encode("barChart", forKey: .type)
            try chart.encode(to: encoder)
        case let .lineChart(chart):
            try container.encode("lineChart", forKey: .type)
            try chart.encode(to: encoder)
        case let .gauge(gauge):
            try container.encode("gauge", forKey: .type)
            try gauge.encode(to: encoder)
        case let .unsupported(kind):
            try container.encode(kind, forKey: .type)
        }
    }
}

public struct PageSpec: Codable, Equatable, Hashable, Sendable {
    public let title: String
    public let back: String?
    public var header: UIPageHeaderSlot?
    public var body: UIPageBodySlot
    public var footer: UIFooterActionsSpec

    public init(
        title: String,
        back: String? = nil,
        header: UIPageHeaderSlot? = nil,
        body: UIPageBodySlot,
        footer: UIFooterActionsSpec = .init()
    ) {
        self.title = title
        self.back = back
        self.header = header
        self.body = body
        self.footer = footer
    }

    enum CodingKeys: String, CodingKey { case title, back, header, body, footer }

    public init(from decoder: Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        title = try container.decode(String.self, forKey: .title)
        back = try container.decodeIfPresent(String.self, forKey: .back)
        header = try container.decodeIfPresent(UIPageHeaderSlot.self, forKey: .header)
        body = try container.decode(UIPageBodySlot.self, forKey: .body)
        footer = try container.decodeIfPresent(UIFooterActionsSpec.self, forKey: .footer) ?? .init()
    }

    public var requiredCapabilities: [String]? {
        guard footer.isValid else { return nil }
        var capabilities = [UnpeelUIProtocol.pageCapability]
        if !footer.actions.isEmpty {
            capabilities.append(UnpeelUIProtocol.footerActionsCapability)
        }
        let chartCapability: String? = switch body {
        case .sparkline: UnpeelUIProtocol.sparklineCapability
        case .barChart: UnpeelUIProtocol.barChartCapability
        case .lineChart: UnpeelUIProtocol.lineChartCapability
        case .gauge: UnpeelUIProtocol.gaugeCapability
        case .list, .content, .unsupported: nil
        }
        if let chartCapability {
            capabilities.append(chartCapability)
            if let header {
                guard case .input = header else { return nil }
                capabilities.append(UnpeelUIProtocol.inputCapability)
            }
            if back != nil { capabilities.append(UnpeelUIProtocol.pageBackCapability) }
            return capabilities
        }
        if case let .content(content) = body {
            capabilities.append(UnpeelUIProtocol.contentCapability)
            if let header {
                guard case .input = header else { return nil }
                capabilities.append(UnpeelUIProtocol.inputCapability)
            }
            if back != nil { capabilities.append(UnpeelUIProtocol.pageBackCapability) }
            if content.selection != nil || content.select != nil {
                capabilities.append(UnpeelUIProtocol.contentSelectionCapability)
            }
            if content.contextMenu != nil {
                capabilities += [
                    UnpeelUIProtocol.menuCapability,
                    UnpeelUIProtocol.menuAnchorCapability,
                ]
            }
            return capabilities
        }
        guard case let .list(list) = body else { return nil }
        capabilities += [
            UnpeelUIProtocol.listCapability,
            UnpeelUIProtocol.listItemCapability,
        ]
        if let header {
            guard case .input = header else { return nil }
            capabilities.append(UnpeelUIProtocol.inputCapability)
        }
        if back != nil { capabilities.append(UnpeelUIProtocol.pageBackCapability) }
        if list.items.contains(where: { $0.detail != nil || $0.value != nil }) {
            capabilities.append(UnpeelUIProtocol.listItemMetadataCapability)
        }
        if list.items.contains(where: { $0.activate != nil }) {
            capabilities.append(UnpeelUIProtocol.listItemActivateCapability)
        }
        if list.items.contains(where: { $0.primaryRole != .static }) {
            capabilities.append(UnpeelUIProtocol.listItemRoleCapability)
        }
        var hasToggle = false
        var hasStatus = false
        var hasBadge = false
        var hasSparkline = false
        var hasGauge = false
        for item in list.items {
            for slot in [item.leading, item.trailing, item.accessory].compactMap({ $0 }) {
                switch slot {
                case .toggle: hasToggle = true
                case .status: hasStatus = true
                case .badge: hasBadge = true
                case .sparkline: hasSparkline = true
                case .gauge: hasGauge = true
                case .disclosure, .checkmark: break
                case .unsupported: return nil
                }
            }
        }
        if hasToggle { capabilities.append(UnpeelUIProtocol.toggleCapability) }
        if list.items.contains(where: {
            $0.busy || $0.labelTone != .default || $0.valueTone != .muted
                || $0.emphasis != .regular || $0.valueMinWidth != nil
                || $0.leading?.kind == "status" || $0.leading?.kind == "badge"
                || $0.trailing?.kind == "status" || $0.trailing?.kind == "badge"
                || $0.accessory?.kind == "status" || $0.accessory?.kind == "badge"
        }) {
            capabilities.append(UnpeelUIProtocol.listItemPresentationCapability)
        }
        if list.items.contains(where: {
            !$0.labelRuns.isEmpty || !$0.detailRuns.isEmpty || !$0.valueRuns.isEmpty
        }) {
            capabilities.append(UnpeelUIProtocol.listItemStyledTextCapability)
        }
        if hasStatus { capabilities.append(UnpeelUIProtocol.statusSymbolCapability) }
        if hasBadge { capabilities.append(UnpeelUIProtocol.badgeCapability) }
        if hasSparkline { capabilities.append(UnpeelUIProtocol.sparklineCapability) }
        if hasGauge { capabilities.append(UnpeelUIProtocol.gaugeCapability) }
        if list.selectedID != nil || list.select != nil || list.scrollPadding != 0
            || list.pageOverlap != 1 || list.pageBehavior != .selection || list.spacePagesDown
        {
            capabilities.append(UnpeelUIProtocol.listSelectionCapability)
        }
        if list.contextMenu != nil {
            capabilities += [
                UnpeelUIProtocol.menuCapability,
                UnpeelUIProtocol.menuAnchorCapability,
            ]
        }
        return capabilities
    }
}

public enum UITreePresentation: String, Codable, Equatable, Hashable, Sendable {
    case drillDown
    case outline
}

public enum UITreeItemKind: String, Codable, Equatable, Hashable, Sendable {
    case parent
    case directory
    case file
}

public enum UITreeChildState: String, Codable, Equatable, Hashable, Sendable {
    case loaded
    case unloaded
    case loading
}

public struct UITreeItem: Codable, Equatable, Hashable, Sendable, Identifiable {
    public let id: String
    public let label: String
    public let kind: UITreeItemKind
    public let detail: String?
    public let hidden: Bool
    public let symlink: Bool
    public var childState: UITreeChildState
    public var expanded: Bool
    public var children: [UITreeItem]

    public init(
        id: String,
        label: String,
        kind: UITreeItemKind,
        detail: String? = nil,
        hidden: Bool = false,
        symlink: Bool = false,
        childState: UITreeChildState = .loaded,
        expanded: Bool = false,
        children: [UITreeItem] = []
    ) {
        self.id = id
        self.label = label
        self.kind = kind
        self.detail = detail
        self.hidden = hidden
        self.symlink = symlink
        self.childState = childState
        self.expanded = expanded
        self.children = children
    }

    enum CodingKeys: String, CodingKey {
        case id, label, kind, detail, hidden, symlink, childState, expanded, children
    }

    public init(from decoder: Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        id = try container.decode(String.self, forKey: .id)
        label = try container.decode(String.self, forKey: .label)
        kind = try container.decode(UITreeItemKind.self, forKey: .kind)
        detail = try container.decodeIfPresent(String.self, forKey: .detail)
        hidden = try container.decodeIfPresent(Bool.self, forKey: .hidden) ?? false
        symlink = try container.decodeIfPresent(Bool.self, forKey: .symlink) ?? false
        childState = try container.decodeIfPresent(
            UITreeChildState.self,
            forKey: .childState
        ) ?? .loaded
        expanded = try container.decodeIfPresent(Bool.self, forKey: .expanded) ?? false
        children = try container.decodeIfPresent([UITreeItem].self, forKey: .children) ?? []
    }
}

public struct UITreeFilter: Codable, Equatable, Hashable, Sendable {
    public let id: String
    public let label: String
    public var value: String
    public let placeholder: String
    public let setValue: String

    public init(
        id: String,
        label: String,
        value: String = "",
        placeholder: String = "",
        setValue: String
    ) {
        self.id = id
        self.label = label
        self.value = value
        self.placeholder = placeholder
        self.setValue = setValue
    }

    enum CodingKeys: String, CodingKey {
        case id, label, value, placeholder, setValue
    }

    public init(from decoder: Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        id = try container.decode(String.self, forKey: .id)
        label = try container.decode(String.self, forKey: .label)
        value = try container.decodeIfPresent(String.self, forKey: .value) ?? ""
        placeholder = try container.decodeIfPresent(String.self, forKey: .placeholder) ?? ""
        setValue = try container.decode(String.self, forKey: .setValue)
    }
}

public struct UITreeActions: Codable, Equatable, Hashable, Sendable {
    public let select: String
    public let open: String
    public let parent: String
    public let setExpanded: String?

    public init(
        select: String = "tree-select",
        open: String = "tree-open",
        parent: String = "tree-parent",
        setExpanded: String? = nil
    ) {
        self.select = select
        self.open = open
        self.parent = parent
        self.setExpanded = setExpanded
    }
}

public struct UITreeSpec: Codable, Equatable, Hashable, Sendable {
    public let label: String
    public var location: String
    public let presentation: UITreePresentation
    public var filter: UITreeFilter?
    public var items: [UITreeItem]
    public var selectedID: String?
    public let emptyMessage: String?
    public let primaryAction: UIButtonSpec?
    public let contextMenu: UIMenuSpec?
    public let actions: UITreeActions
    public var footer: UIFooterActionsSpec

    public init(
        label: String,
        location: String,
        presentation: UITreePresentation = .drillDown,
        filter: UITreeFilter? = nil,
        items: [UITreeItem],
        selectedID: String? = nil,
        emptyMessage: String? = nil,
        primaryAction: UIButtonSpec? = nil,
        contextMenu: UIMenuSpec? = nil,
        actions: UITreeActions = .init(),
        footer: UIFooterActionsSpec = .init()
    ) {
        self.label = label
        self.location = location
        self.presentation = presentation
        self.filter = filter
        self.items = items
        self.selectedID = selectedID
        self.emptyMessage = emptyMessage
        self.primaryAction = primaryAction
        self.contextMenu = contextMenu
        self.actions = actions
        self.footer = footer
    }

    enum CodingKeys: String, CodingKey {
        case label, location, presentation, filter, items, selectedID = "selectedId"
        case emptyMessage, primaryAction, contextMenu, actions, footer
    }

    public init(from decoder: Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        label = try container.decode(String.self, forKey: .label)
        location = try container.decode(String.self, forKey: .location)
        presentation = try container.decodeIfPresent(
            UITreePresentation.self,
            forKey: .presentation
        ) ?? .drillDown
        filter = try container.decodeIfPresent(UITreeFilter.self, forKey: .filter)
        items = try container.decodeIfPresent([UITreeItem].self, forKey: .items) ?? []
        selectedID = try container.decodeIfPresent(String.self, forKey: .selectedID)
        emptyMessage = try container.decodeIfPresent(String.self, forKey: .emptyMessage)
        primaryAction = try container.decodeIfPresent(UIButtonSpec.self, forKey: .primaryAction)
        contextMenu = try container.decodeIfPresent(UIMenuSpec.self, forKey: .contextMenu)
        actions = try container.decodeIfPresent(UITreeActions.self, forKey: .actions) ?? .init()
        footer = try container.decodeIfPresent(UIFooterActionsSpec.self, forKey: .footer) ?? .init()
    }

    public var requiredCapabilities: [String]? {
        var ids = Set<String>()
        var count = 0
        var parentCount = 0
        guard validateTreeItems(
            items,
            depth: 0,
            ids: &ids,
            count: &count,
            parentCount: &parentCount
        ), parentCount <= 1,
              selectedID.map(ids.contains) ?? true,
              presentation != .outline || actions.setExpanded != nil,
              footer.isValid,
              footer.actions.allSatisfy({ !ids.contains($0.id) })
        else { return nil }
        var capabilities = [UnpeelUIProtocol.treeCapability]
        if presentation == .outline || items.contains(where: { !$0.children.isEmpty }) {
            capabilities.append(UnpeelUIProtocol.treeHierarchyCapability)
        }
        if filter != nil { capabilities.append(UnpeelUIProtocol.treeFilterCapability) }
        if parentCount > 0 { capabilities.append(UnpeelUIProtocol.treeParentCapability) }
        if primaryAction != nil { capabilities.append(UnpeelUIProtocol.buttonCapability) }
        if contextMenu != nil {
            capabilities += [
                UnpeelUIProtocol.menuCapability,
                UnpeelUIProtocol.menuAnchorCapability,
            ]
        }
        if !footer.actions.isEmpty {
            capabilities.append(UnpeelUIProtocol.footerActionsCapability)
        }
        return capabilities
    }
}

private func validateTreeItems(
    _ items: [UITreeItem],
    depth: Int,
    ids: inout Set<String>,
    count: inout Int,
    parentCount: inout Int
) -> Bool {
    guard depth <= 32 else { return false }
    for item in items {
        count += 1
        guard count <= 100_000, ids.insert(item.id).inserted,
              !item.label.contains("\n"), !item.label.contains("\r")
        else { return false }
        switch item.kind {
        case .parent:
            parentCount += 1
            guard depth == 0, item.children.isEmpty, !item.expanded else { return false }
        case .file:
            guard item.children.isEmpty, !item.expanded else { return false }
        case .directory:
            guard item.childState == .loaded || item.children.isEmpty,
                  validateTreeItems(
                      item.children,
                      depth: depth + 1,
                      ids: &ids,
                      count: &count,
                      parentCount: &parentCount
                  )
            else { return false }
        }
    }
    return true
}

public enum TextBoxTitlePosition: String, Codable, Equatable, Hashable, Sendable {
    case topLeft
    case topRight
    case bottomLeft
    case bottomRight
}

public enum TextBoxSubmitMode: String, Codable, Equatable, Hashable, Sendable {
    case enter
    case never
}

public struct TextBoxTitle: Codable, Equatable, Hashable, Sendable {
    public let text: String
    public let position: TextBoxTitlePosition

    public init(text: String, position: TextBoxTitlePosition) {
        self.text = text
        self.position = position
    }
}

public struct TextBoxKeyHint: Codable, Equatable, Hashable, Sendable {
    public let key: String
    public let label: String

    public init(key: String, label: String) {
        self.key = key
        self.label = label
    }
}

public struct TextBoxBusy: Codable, Equatable, Hashable, Sendable {
    public let label: String
    public let elapsedMs: UInt64
    public let rightMeta: String

    public init(label: String, elapsedMs: UInt64 = 0, rightMeta: String = "") {
        self.label = label
        self.elapsedMs = elapsedMs
        self.rightMeta = rightMeta
    }

    enum CodingKeys: String, CodingKey {
        case label
        case elapsedMs
        case rightMeta
    }

    public init(from decoder: Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        label = try container.decode(String.self, forKey: .label)
        elapsedMs = try container.decodeIfPresent(UInt64.self, forKey: .elapsedMs) ?? 0
        rightMeta = try container.decodeIfPresent(String.self, forKey: .rightMeta) ?? ""
    }

    public func encode(to encoder: Encoder) throws {
        var container = encoder.container(keyedBy: CodingKeys.self)
        try container.encode(label, forKey: .label)
        try container.encode(elapsedMs, forKey: .elapsedMs)
        if !rightMeta.isEmpty { try container.encode(rightMeta, forKey: .rightMeta) }
    }
}

public struct TextBoxActions: Codable, Equatable, Hashable, Sendable {
    public let setText: String?
    public let submit: String?

    public init(setText: String? = nil, submit: String? = nil) {
        self.setText = setText
        self.submit = submit
    }
}

/// Closed multi-line text box root: text plus optional prompt chrome.
public struct TextBoxSpec: Codable, Equatable, Hashable, Sendable {
    public let text: String
    public let placeholder: String
    public let prompt: String
    public let titles: [TextBoxTitle]
    public let hints: [TextBoxKeyHint]
    public let busy: TextBoxBusy?
    public let submitMode: TextBoxSubmitMode
    public let minRows: Int
    public let maxRows: Int
    public let actions: TextBoxActions

    public init(
        text: String = "",
        placeholder: String = "",
        prompt: String = "",
        titles: [TextBoxTitle] = [],
        hints: [TextBoxKeyHint] = [],
        busy: TextBoxBusy? = nil,
        submitMode: TextBoxSubmitMode = .enter,
        minRows: Int = 3,
        maxRows: Int = 10,
        actions: TextBoxActions = TextBoxActions()
    ) {
        self.text = text
        self.placeholder = placeholder
        self.prompt = prompt
        self.titles = titles
        self.hints = hints
        self.busy = busy
        self.submitMode = submitMode
        self.minRows = minRows
        self.maxRows = maxRows
        self.actions = actions
    }

    enum CodingKeys: String, CodingKey {
        case text
        case placeholder
        case prompt
        case titles
        case hints
        case busy
        case submitMode
        case minRows
        case maxRows
        case actions
    }

    public init(from decoder: Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        text = try container.decodeIfPresent(String.self, forKey: .text) ?? ""
        placeholder = try container.decodeIfPresent(String.self, forKey: .placeholder) ?? ""
        prompt = try container.decodeIfPresent(String.self, forKey: .prompt) ?? ""
        titles = try container.decodeIfPresent([TextBoxTitle].self, forKey: .titles) ?? []
        hints = try container.decodeIfPresent([TextBoxKeyHint].self, forKey: .hints) ?? []
        busy = try container.decodeIfPresent(TextBoxBusy.self, forKey: .busy)
        submitMode = try container.decodeIfPresent(TextBoxSubmitMode.self, forKey: .submitMode)
            ?? .enter
        minRows = try container.decodeIfPresent(Int.self, forKey: .minRows) ?? 3
        maxRows = try container.decodeIfPresent(Int.self, forKey: .maxRows) ?? 10
        actions = try container.decodeIfPresent(TextBoxActions.self, forKey: .actions)
            ?? TextBoxActions()
        guard minRows >= 1, maxRows >= minRows else {
            throw DecodingError.dataCorruptedError(
                forKey: .minRows,
                in: container,
                debugDescription: "TextBox minRows must be at least 1 and at most maxRows"
            )
        }
    }

    public func encode(to encoder: Encoder) throws {
        var container = encoder.container(keyedBy: CodingKeys.self)
        try container.encode(text, forKey: .text)
        if !placeholder.isEmpty { try container.encode(placeholder, forKey: .placeholder) }
        if !prompt.isEmpty { try container.encode(prompt, forKey: .prompt) }
        if !titles.isEmpty { try container.encode(titles, forKey: .titles) }
        if !hints.isEmpty { try container.encode(hints, forKey: .hints) }
        try container.encodeIfPresent(busy, forKey: .busy)
        try container.encode(submitMode, forKey: .submitMode)
        try container.encode(minRows, forKey: .minRows)
        try container.encode(maxRows, forKey: .maxRows)
        try container.encode(actions, forKey: .actions)
    }

    public var isValid: Bool {
        titles.count == Set(titles.map(\.position)).count
    }
}

public enum UIComponent: Equatable, Sendable {
    case canvasPage(CanvasPageSpec)
    case markdownEditor(MarkdownEditorSpec)
    case media(MediaSpec)
    case menu(UIMenuSpec)
    case page(PageSpec)
    case surface(SurfaceSpec)
    case textBox(TextBoxSpec)
    case tree(UITreeSpec)
    case unsupported(kind: String)

    public var kind: String {
        switch self {
        case .canvasPage:
            "canvasPage"
        case .markdownEditor:
            "markdownEditor"
        case .media:
            "media"
        case .menu:
            "menu"
        case .page:
            "page"
        case .surface:
            "surface"
        case .textBox:
            "textBox"
        case .tree:
            "tree"
        case let .unsupported(kind):
            kind
        }
    }

    public var requiredCapability: String? {
        requiredCapabilities?.first
    }

    public var requiredCapabilities: [String]? {
        switch self {
        case let .canvasPage(page):
            return page.requiredCapabilities
        case let .markdownEditor(editor):
            guard editor.insertMenu?.requiredCapabilities != nil || editor.insertMenu == nil,
                  editor.contextMenu?.requiredCapabilities != nil || editor.contextMenu == nil,
                  editor.commandHint?.isValid ?? true,
                  editor.commandHint == nil || editor.actions.openMenu != nil,
                  editor.footer.isValid
            else { return nil }
            var capabilities = [UnpeelUIProtocol.markdownEditorCapability]
            if editor.commandHint != nil {
                capabilities.append(UnpeelUIProtocol.markdownCommandHintCapability)
            }
            if editor.insertMenu != nil || editor.contextMenu != nil {
                capabilities += [
                    UnpeelUIProtocol.menuCapability,
                    UnpeelUIProtocol.menuAnchorCapability,
                ]
            }
            if !editor.footer.actions.isEmpty {
                capabilities.append(UnpeelUIProtocol.footerActionsCapability)
            }
            return capabilities
        case .media:
            return [UnpeelUIProtocol.mediaCapability]
        case let .menu(menu):
            return menu.requiredCapabilities
        case let .page(page):
            return page.requiredCapabilities
        case .surface:
            return [UnpeelUIProtocol.surfaceCapability]
        case let .textBox(textBox):
            guard textBox.isValid else { return nil }
            return [UnpeelUIProtocol.textBoxCapability]
        case let .tree(tree):
            return tree.requiredCapabilities
        case .unsupported:
            return nil
        }
    }
}

public struct UINode: Equatable, Sendable {
    public let id: String
    public let component: UIComponent

    public init(id: String, component: UIComponent) {
        self.id = id
        self.component = component
    }
}

extension UINode: Codable {
    enum CodingKeys: String, CodingKey {
        case id
        case type
    }

    public init(from decoder: Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        id = try container.decode(String.self, forKey: .id)
        let kind = try container.decode(String.self, forKey: .type)
        switch kind {
        case "canvasPage":
            component = .canvasPage(try CanvasPageSpec(from: decoder))
        case "markdownEditor":
            component = .markdownEditor(try MarkdownEditorSpec(from: decoder))
        case "media":
            component = .media(try MediaSpec(from: decoder))
        case "menu":
            component = .menu(try UIMenuSpec(from: decoder))
        case "page":
            component = .page(try PageSpec(from: decoder))
        case "surface":
            component = .surface(try SurfaceSpec(from: decoder))
        case "textBox":
            component = .textBox(try TextBoxSpec(from: decoder))
        case "tree":
            component = .tree(try UITreeSpec(from: decoder))
        default:
            component = .unsupported(kind: kind)
        }
    }

    public func encode(to encoder: Encoder) throws {
        var container = encoder.container(keyedBy: CodingKeys.self)
        try container.encode(id, forKey: .id)
        switch component {
        case let .canvasPage(page):
            try container.encode("canvasPage", forKey: .type)
            try page.encode(to: encoder)
        case let .markdownEditor(editor):
            try container.encode("markdownEditor", forKey: .type)
            try editor.encode(to: encoder)
        case let .media(media):
            try container.encode("media", forKey: .type)
            try media.encode(to: encoder)
        case let .menu(menu):
            try container.encode("menu", forKey: .type)
            try menu.encode(to: encoder)
        case let .page(page):
            try container.encode("page", forKey: .type)
            try page.encode(to: encoder)
        case let .surface(surface):
            try container.encode("surface", forKey: .type)
            try surface.encode(to: encoder)
        case let .textBox(textBox):
            try container.encode("textBox", forKey: .type)
            try textBox.encode(to: encoder)
        case let .tree(tree):
            try container.encode("tree", forKey: .type)
            try tree.encode(to: encoder)
        case let .unsupported(kind):
            try container.encode(kind, forKey: .type)
        }
    }
}

public struct UISnapshot: Codable, Equatable, Sendable {
    public let protocolName: String
    public let protocolVersion: Int
    public let appInstanceID: String
    public let clientID: String
    public let viewID: String
    public let revision: Int
    public let root: UINode

    public init(
        protocolVersion: Int = UnpeelUIProtocol.version,
        appInstanceID: String,
        clientID: String,
        viewID: String,
        revision: Int,
        root: UINode
    ) {
        protocolName = UnpeelUIProtocol.name
        self.protocolVersion = protocolVersion
        self.appInstanceID = appInstanceID
        self.clientID = clientID
        self.viewID = viewID
        self.revision = revision
        self.root = root
    }

    enum CodingKeys: String, CodingKey {
        case protocolName = "protocol"
        case protocolVersion
        case appInstanceID = "appInstanceId"
        case clientID = "clientId"
        case viewID = "viewId"
        case revision
        case root
    }
}

public enum UIEventKind: String, Codable, Equatable, Sendable {
    case activate
    case select
    case change
    case submit
    case cancel
    case command
}

public enum UIEventValue: Equatable, Sendable {
    case none
    case bool(Bool)
    case index(Int)
    case integer(Int)
    case number(Double)
    case text(String)
    case textList([String])
    case textEdit(UITextEdit)
    case textSelection(UITextSelection)
}

extension UIEventValue: Codable {
    enum CodingKeys: String, CodingKey {
        case type
        case value
    }

    enum ValueType: String, Codable {
        case none
        case bool
        case index
        case integer
        case number
        case text
        case textList
        case textEdit
        case textSelection
    }

    public init(from decoder: Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        switch try container.decode(ValueType.self, forKey: .type) {
        case .none:
            self = .none
        case .bool:
            self = .bool(try container.decode(Bool.self, forKey: .value))
        case .index:
            self = .index(try container.decode(Int.self, forKey: .value))
        case .integer:
            self = .integer(try container.decode(Int.self, forKey: .value))
        case .number:
            self = .number(try container.decode(Double.self, forKey: .value))
        case .text:
            self = .text(try container.decode(String.self, forKey: .value))
        case .textList:
            self = .textList(try container.decode([String].self, forKey: .value))
        case .textEdit:
            self = .textEdit(try container.decode(UITextEdit.self, forKey: .value))
        case .textSelection:
            self = .textSelection(try container.decode(UITextSelection.self, forKey: .value))
        }
    }

    public func encode(to encoder: Encoder) throws {
        var container = encoder.container(keyedBy: CodingKeys.self)
        switch self {
        case .none:
            try container.encode(ValueType.none, forKey: .type)
        case let .bool(value):
            try container.encode(ValueType.bool, forKey: .type)
            try container.encode(value, forKey: .value)
        case let .index(value):
            try container.encode(ValueType.index, forKey: .type)
            try container.encode(value, forKey: .value)
        case let .integer(value):
            try container.encode(ValueType.integer, forKey: .type)
            try container.encode(value, forKey: .value)
        case let .number(value):
            try container.encode(ValueType.number, forKey: .type)
            try container.encode(value, forKey: .value)
        case let .text(value):
            try container.encode(ValueType.text, forKey: .type)
            try container.encode(value, forKey: .value)
        case let .textList(value):
            try container.encode(ValueType.textList, forKey: .type)
            try container.encode(value, forKey: .value)
        case let .textEdit(value):
            try container.encode(ValueType.textEdit, forKey: .type)
            try container.encode(value, forKey: .value)
        case let .textSelection(value):
            try container.encode(ValueType.textSelection, forKey: .type)
            try container.encode(value, forKey: .value)
        }
    }
}

/// Renderer-local action. The session transport applies identity and revision.
public struct UIAction: Codable, Equatable, Sendable {
    public let nodeID: String
    public let action: String
    public let kind: UIEventKind
    public let value: UIEventValue

    public init(
        nodeID: String,
        action: String,
        kind: UIEventKind,
        value: UIEventValue = .none
    ) {
        self.nodeID = nodeID
        self.action = action
        self.kind = kind
        self.value = value
    }

    enum CodingKeys: String, CodingKey {
        case nodeID = "nodeId"
        case action
        case kind
        case value
    }
}

public struct UIEvent: Equatable, Sendable {
    public let protocolName: String
    public let protocolVersion: Int
    public let appInstanceID: String
    public let participantID: String
    public let clientID: String
    public let rendererID: String
    public let viewID: String
    public let eventID: String
    public let baseRevision: Int
    public let action: UIAction

    public init(
        snapshot: UISnapshot,
        participantID: String,
        rendererID: String,
        eventID: String = UUID().uuidString.lowercased(),
        action: UIAction
    ) {
        protocolName = UnpeelUIProtocol.name
        protocolVersion = snapshot.protocolVersion
        appInstanceID = snapshot.appInstanceID
        self.participantID = participantID
        clientID = snapshot.clientID
        self.rendererID = rendererID
        viewID = snapshot.viewID
        self.eventID = eventID
        baseRevision = snapshot.revision
        self.action = action
    }
}

extension UIEvent: Codable {
    enum CodingKeys: String, CodingKey {
        case protocolName = "protocol"
        case protocolVersion
        case appInstanceID = "appInstanceId"
        case participantID = "participantId"
        case clientID = "clientId"
        case rendererID = "rendererId"
        case viewID = "viewId"
        case eventID = "eventId"
        case baseRevision
    }

    public init(from decoder: Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        protocolName = try container.decode(String.self, forKey: .protocolName)
        protocolVersion = try container.decode(Int.self, forKey: .protocolVersion)
        appInstanceID = try container.decode(String.self, forKey: .appInstanceID)
        participantID = try container.decode(String.self, forKey: .participantID)
        clientID = try container.decode(String.self, forKey: .clientID)
        rendererID = try container.decode(String.self, forKey: .rendererID)
        viewID = try container.decode(String.self, forKey: .viewID)
        eventID = try container.decode(String.self, forKey: .eventID)
        baseRevision = try container.decode(Int.self, forKey: .baseRevision)
        action = try UIAction(from: decoder)
    }

    public func encode(to encoder: Encoder) throws {
        var container = encoder.container(keyedBy: CodingKeys.self)
        try container.encode(protocolName, forKey: .protocolName)
        try container.encode(protocolVersion, forKey: .protocolVersion)
        try container.encode(appInstanceID, forKey: .appInstanceID)
        try container.encode(participantID, forKey: .participantID)
        try container.encode(clientID, forKey: .clientID)
        try container.encode(rendererID, forKey: .rendererID)
        try container.encode(viewID, forKey: .viewID)
        try container.encode(eventID, forKey: .eventID)
        try container.encode(baseRevision, forKey: .baseRevision)
        try action.encode(to: encoder)
    }
}

public enum UIAckStatus: String, Codable, Equatable, Sendable {
    case pending
    case applied
    case rejected
    case stale
}

public struct UIAck: Codable, Equatable, Sendable {
    public let protocolName: String
    public let protocolVersion: Int
    public let appInstanceID: String
    public let clientID: String
    public let rendererID: String
    public let viewID: String
    public let eventID: String
    public let status: UIAckStatus
    public let revision: Int
    public let message: String?

    enum CodingKeys: String, CodingKey {
        case protocolName = "protocol"
        case protocolVersion
        case appInstanceID = "appInstanceId"
        case clientID = "clientId"
        case rendererID = "rendererId"
        case viewID = "viewId"
        case eventID = "eventId"
        case status
        case revision
        case message
    }
}

public struct UILifecycle: Codable, Equatable, Sendable {
    public let protocolName: String
    public let protocolVersion: Int
    public let appInstanceID: String
    public let clientID: String
    public let rendererID: String
    public let viewID: String
    public let state: UIRendererState

    public init(snapshot: UISnapshot, rendererID: String, state: UIRendererState) {
        protocolName = UnpeelUIProtocol.name
        protocolVersion = snapshot.protocolVersion
        appInstanceID = snapshot.appInstanceID
        clientID = snapshot.clientID
        self.rendererID = rendererID
        viewID = snapshot.viewID
        self.state = state
    }

    enum CodingKeys: String, CodingKey {
        case protocolName = "protocol"
        case protocolVersion
        case appInstanceID = "appInstanceId"
        case clientID = "clientId"
        case rendererID = "rendererId"
        case viewID = "viewId"
        case state
    }
}

public struct UIRequestSnapshot: Codable, Equatable, Sendable {
    public let protocolName: String
    public let protocolVersion: Int
    public let appInstanceID: String
    public let clientID: String
    public let rendererID: String
    public let viewID: String

    enum CodingKeys: String, CodingKey {
        case protocolName = "protocol"
        case protocolVersion
        case appInstanceID = "appInstanceId"
        case clientID = "clientId"
        case rendererID = "rendererId"
        case viewID = "viewId"
    }
}

public struct UIPresenceMember: Codable, Equatable, Sendable {
    public let participant: UIParticipant
    public let clientID: String
    public let renderer: UIRendererMetadata
    public let state: UIRendererState

    enum CodingKeys: String, CodingKey {
        case participant
        case clientID = "clientId"
        case renderer
        case state
    }
}

public struct UIPresence: Codable, Equatable, Sendable {
    public let protocolName: String
    public let protocolVersion: Int
    public let appInstanceID: String
    public let viewID: String
    public let members: [UIPresenceMember]

    enum CodingKeys: String, CodingKey {
        case protocolName = "protocol"
        case protocolVersion
        case appInstanceID = "appInstanceId"
        case viewID = "viewId"
        case members
    }
}

public struct UIErrorMessage: Codable, Equatable, Sendable {
    public let protocolName: String
    public let protocolVersion: Int
    public let code: String
    public let message: String

    enum CodingKeys: String, CodingKey {
        case protocolName = "protocol"
        case protocolVersion
        case code
        case message
    }
}

public enum UIMessage: Equatable, Sendable {
    case attach(UIAttach)
    case attached(UIAttached)
    case snapshot(UISnapshot)
    case delta(UIDelta)
    case event(UIEvent)
    case ack(UIAck)
    case lifecycle(UILifecycle)
    case requestSnapshot(UIRequestSnapshot)
    case presence(UIPresence)
    case error(UIErrorMessage)
}

extension UIMessage: Codable {
    enum CodingKeys: String, CodingKey {
        case type
        case protocolName = "protocol"
        case protocolVersion
    }

    enum MessageType: String, Codable, Equatable {
        case attach
        case attached
        case snapshot
        case delta
        case event
        case ack
        case lifecycle
        case requestSnapshot
        case presence
        case error
    }

    public init(from decoder: Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        let protocolName = try container.decode(String.self, forKey: .protocolName)
        guard protocolName == UnpeelUIProtocol.name else {
            throw DecodingError.dataCorruptedError(
                forKey: .protocolName,
                in: container,
                debugDescription: "Unsupported UI protocol \(protocolName)"
            )
        }
        let messageType = try container.decode(MessageType.self, forKey: .type)
        if messageType != .attach {
            let protocolVersion = try container.decode(Int.self, forKey: .protocolVersion)
            guard UnpeelUIProtocol.supports(protocolVersion) else {
                throw DecodingError.dataCorruptedError(
                    forKey: .protocolVersion,
                    in: container,
                    debugDescription: "Unsupported UI protocol version \(protocolVersion)"
                )
            }
        }
        switch messageType {
        case .attach:
            self = .attach(try UIAttach(from: decoder))
        case .attached:
            self = .attached(try UIAttached(from: decoder))
        case .snapshot:
            self = .snapshot(try UISnapshot(from: decoder))
        case .delta:
            self = .delta(try UIDelta(from: decoder))
        case .event:
            self = .event(try UIEvent(from: decoder))
        case .ack:
            self = .ack(try UIAck(from: decoder))
        case .lifecycle:
            self = .lifecycle(try UILifecycle(from: decoder))
        case .requestSnapshot:
            self = .requestSnapshot(try UIRequestSnapshot(from: decoder))
        case .presence:
            self = .presence(try UIPresence(from: decoder))
        case .error:
            self = .error(try UIErrorMessage(from: decoder))
        }
    }

    public func encode(to encoder: Encoder) throws {
        var container = encoder.container(keyedBy: CodingKeys.self)
        switch self {
        case let .attach(message):
            try container.encode(MessageType.attach, forKey: .type)
            try message.encode(to: encoder)
        case let .attached(message):
            try container.encode(MessageType.attached, forKey: .type)
            try message.encode(to: encoder)
        case let .snapshot(message):
            try container.encode(MessageType.snapshot, forKey: .type)
            try message.encode(to: encoder)
        case let .delta(message):
            try container.encode(MessageType.delta, forKey: .type)
            try message.encode(to: encoder)
        case let .event(message):
            try container.encode(MessageType.event, forKey: .type)
            try message.encode(to: encoder)
        case let .ack(message):
            try container.encode(MessageType.ack, forKey: .type)
            try message.encode(to: encoder)
        case let .lifecycle(message):
            try container.encode(MessageType.lifecycle, forKey: .type)
            try message.encode(to: encoder)
        case let .requestSnapshot(message):
            try container.encode(MessageType.requestSnapshot, forKey: .type)
            try message.encode(to: encoder)
        case let .presence(message):
            try container.encode(MessageType.presence, forKey: .type)
            try message.encode(to: encoder)
        case let .error(message):
            try container.encode(MessageType.error, forKey: .type)
            try message.encode(to: encoder)
        }
    }
}

public extension UIMessage {
    /// The selected connection version, or `nil` for the range-bearing attach.
    var protocolVersion: Int? {
        switch self {
        case .attach:
            nil
        case let .attached(message):
            message.protocolVersion
        case let .snapshot(message):
            message.protocolVersion
        case let .delta(message):
            message.protocolVersion
        case let .event(message):
            message.protocolVersion
        case let .ack(message):
            message.protocolVersion
        case let .lifecycle(message):
            message.protocolVersion
        case let .requestSnapshot(message):
            message.protocolVersion
        case let .presence(message):
            message.protocolVersion
        case let .error(message):
            message.protocolVersion
        }
    }
}
