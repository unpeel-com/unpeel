import Foundation

/// One ordered server-side mutation between immutable UI revisions.
public enum UIDeltaOperation: Equatable, Sendable {
    case replaceRoot(UINode)
    case markdownReplaceRange(nodeID: String, edit: UITextEdit)
    case markdownSetSelection(nodeID: String, selection: UITextSelection)
    case markdownSetPresentation(nodeID: String, presentation: MarkdownPresentation)
    case markdownSetDirty(nodeID: String, dirty: Bool)
    case markdownSetReadOnly(nodeID: String, readOnly: Bool)
    case markdownSetTitle(nodeID: String, title: String?)
    case markdownSetPlaceholder(nodeID: String, placeholder: String)
    case markdownSetCommandHint(nodeID: String, commandHint: MarkdownCommandHint?)
    case markdownSetActions(nodeID: String, actions: MarkdownEditorActions)
    case markdownSetMenus(nodeID: String, insertMenu: UIMenuSpec?, contextMenu: UIMenuSpec?)
    case menuSetSelection(nodeID: String, selectedID: String?)
    case mediaSetSource(nodeID: String, source: MediaSource, intrinsic: MediaPixelSize)
    case surfaceSetReference(nodeID: String, reference: SurfaceReference)
    case toggleSetValue(nodeID: String, value: Bool)
    case checkmarkSetValue(nodeID: String, value: Bool)
    case sparklineSetData(UISparklineSpec)
    case barChartSetData(UIBarChartSpec)
    case lineChartSetData(UILineChartSpec)
    case gaugeSetData(UIGaugeSpec)
    case footerSetActions(nodeID: String, actions: [UIFooterActionSpec])
    case inputSetValue(nodeID: String, value: String)
    case listInsertItem(listID: String, index: Int, item: UIListItemSpec)
    case listSetSelection(listID: String, selectedID: String?)
    case listRemoveItem(listID: String, itemID: String)
    case contentSetSelection(contentID: String, selection: UIContentSelection?)
    case contentSpliceLines(
        contentID: String,
        index: Int,
        deleteCount: Int,
        lines: [UIContentLine]
    )
    case treeSetSelection(nodeID: String, selectedID: String?)
    case treeSetFilter(filterID: String, value: String)
    case treeSetLocation(nodeID: String, location: String)
    case treeSpliceChildren(
        nodeID: String,
        parentID: String?,
        index: Int,
        deleteCount: Int,
        items: [UITreeItem]
    )
    case treeSetChildState(nodeID: String, itemID: String, childState: UITreeChildState)
    case treeSetExpanded(nodeID: String, itemID: String, expanded: Bool)
}

extension UIDeltaOperation: Codable {
    enum CodingKeys: String, CodingKey {
        case op
        case root
        case nodeID = "nodeId"
        case edit
        case selection
        case presentation
        case dirty
        case readOnly
        case title
        case placeholder
        case commandHint
        case actions
        case insertMenu
        case contextMenu
        case source
        case intrinsic
        case reference
        case value
        case series
        case min
        case max
        case caption
        case unit
        case accessibilityText
        case bars
        case xAxis
        case yAxis
        case ratio
        case label
        case listID = "listId"
        case contentID = "contentId"
        case index
        case item
        case itemID = "itemId"
        case selectedID = "selectedId"
        case filterID = "filterId"
        case location
        case parentID = "parentId"
        case deleteCount
        case items
        case lines
        case childState
        case expanded
    }

    enum Operation: String, Codable {
        case replaceRoot
        case markdownReplaceRange
        case markdownSetSelection
        case markdownSetPresentation
        case markdownSetDirty
        case markdownSetReadOnly
        case markdownSetTitle
        case markdownSetPlaceholder
        case markdownSetCommandHint
        case markdownSetActions
        case markdownSetMenus
        case menuSetSelection
        case mediaSetSource
        case surfaceSetReference
        case toggleSetValue
        case checkmarkSetValue
        case sparklineSetData
        case barChartSetData
        case lineChartSetData
        case gaugeSetData
        case footerSetActions
        case inputSetValue
        case listInsertItem
        case listSetSelection
        case listRemoveItem
        case contentSetSelection
        case contentSpliceLines
        case treeSetSelection
        case treeSetFilter
        case treeSetLocation
        case treeSpliceChildren
        case treeSetChildState
        case treeSetExpanded
    }

    public init(from decoder: Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        switch try container.decode(Operation.self, forKey: .op) {
        case .replaceRoot:
            self = .replaceRoot(try container.decode(UINode.self, forKey: .root))
        case .markdownReplaceRange:
            self = .markdownReplaceRange(
                nodeID: try container.decode(String.self, forKey: .nodeID),
                edit: try container.decode(UITextEdit.self, forKey: .edit)
            )
        case .markdownSetSelection:
            self = .markdownSetSelection(
                nodeID: try container.decode(String.self, forKey: .nodeID),
                selection: try container.decode(UITextSelection.self, forKey: .selection)
            )
        case .markdownSetPresentation:
            self = .markdownSetPresentation(
                nodeID: try container.decode(String.self, forKey: .nodeID),
                presentation: try container.decode(
                    MarkdownPresentation.self,
                    forKey: .presentation
                )
            )
        case .markdownSetDirty:
            self = .markdownSetDirty(
                nodeID: try container.decode(String.self, forKey: .nodeID),
                dirty: try container.decode(Bool.self, forKey: .dirty)
            )
        case .markdownSetReadOnly:
            self = .markdownSetReadOnly(
                nodeID: try container.decode(String.self, forKey: .nodeID),
                readOnly: try container.decode(Bool.self, forKey: .readOnly)
            )
        case .markdownSetTitle:
            self = .markdownSetTitle(
                nodeID: try container.decode(String.self, forKey: .nodeID),
                title: try container.decodeIfPresent(String.self, forKey: .title)
            )
        case .markdownSetPlaceholder:
            self = .markdownSetPlaceholder(
                nodeID: try container.decode(String.self, forKey: .nodeID),
                placeholder: try container.decode(String.self, forKey: .placeholder)
            )
        case .markdownSetCommandHint:
            self = .markdownSetCommandHint(
                nodeID: try container.decode(String.self, forKey: .nodeID),
                commandHint: try container.decodeIfPresent(
                    MarkdownCommandHint.self,
                    forKey: .commandHint
                )
            )
        case .markdownSetActions:
            self = .markdownSetActions(
                nodeID: try container.decode(String.self, forKey: .nodeID),
                actions: try container.decode(MarkdownEditorActions.self, forKey: .actions)
            )
        case .markdownSetMenus:
            self = .markdownSetMenus(
                nodeID: try container.decode(String.self, forKey: .nodeID),
                insertMenu: try container.decodeIfPresent(UIMenuSpec.self, forKey: .insertMenu),
                contextMenu: try container.decodeIfPresent(UIMenuSpec.self, forKey: .contextMenu)
            )
        case .menuSetSelection:
            self = .menuSetSelection(
                nodeID: try container.decode(String.self, forKey: .nodeID),
                selectedID: try container.decodeIfPresent(String.self, forKey: .selectedID)
            )
        case .mediaSetSource:
            self = .mediaSetSource(
                nodeID: try container.decode(String.self, forKey: .nodeID),
                source: try container.decode(MediaSource.self, forKey: .source),
                intrinsic: try container.decode(MediaPixelSize.self, forKey: .intrinsic)
            )
        case .surfaceSetReference:
            self = .surfaceSetReference(
                nodeID: try container.decode(String.self, forKey: .nodeID),
                reference: try container.decode(SurfaceReference.self, forKey: .reference)
            )
        case .toggleSetValue:
            self = .toggleSetValue(
                nodeID: try container.decode(String.self, forKey: .nodeID),
                value: try container.decode(Bool.self, forKey: .value)
            )
        case .checkmarkSetValue:
            self = .checkmarkSetValue(
                nodeID: try container.decode(String.self, forKey: .nodeID),
                value: try container.decode(Bool.self, forKey: .value)
            )
        case .sparklineSetData:
            let sparkline = UISparklineSpec(
                id: try container.decode(String.self, forKey: .nodeID),
                series: try container.decode([Double].self, forKey: .series),
                min: try container.decodeIfPresent(Double.self, forKey: .min),
                max: try container.decodeIfPresent(Double.self, forKey: .max),
                caption: try container.decodeIfPresent(String.self, forKey: .caption),
                unit: try container.decodeIfPresent(String.self, forKey: .unit),
                accessibilityText: try container.decode(String.self, forKey: .accessibilityText)
            )
            guard sparkline.isValid else {
                throw DecodingError.dataCorruptedError(
                    forKey: .series,
                    in: container,
                    debugDescription: "Sparkline delta data is invalid"
                )
            }
            self = .sparklineSetData(sparkline)
        case .barChartSetData:
            let chart = UIBarChartSpec(
                id: try container.decode(String.self, forKey: .nodeID),
                bars: try container.decode([UIBarChartBar].self, forKey: .bars),
                accessibilityText: try container.decode(String.self, forKey: .accessibilityText)
            )
            guard chart.isValid else {
                throw DecodingError.dataCorruptedError(
                    forKey: .bars,
                    in: container,
                    debugDescription: "BarChart delta data is invalid"
                )
            }
            self = .barChartSetData(chart)
        case .lineChartSetData:
            let chart = UILineChartSpec(
                id: try container.decode(String.self, forKey: .nodeID),
                series: try container.decode([UILineChartSeries].self, forKey: .series),
                xAxis: try container.decode(UILineChartAxis.self, forKey: .xAxis),
                yAxis: try container.decode(UILineChartAxis.self, forKey: .yAxis),
                accessibilityText: try container.decode(String.self, forKey: .accessibilityText)
            )
            guard chart.isValid else {
                throw DecodingError.dataCorruptedError(
                    forKey: .series,
                    in: container,
                    debugDescription: "LineChart delta data is invalid"
                )
            }
            self = .lineChartSetData(chart)
        case .gaugeSetData:
            let gauge = UIGaugeSpec(
                id: try container.decode(String.self, forKey: .nodeID),
                ratio: try container.decode(Double.self, forKey: .ratio),
                label: try container.decode(String.self, forKey: .label),
                caption: try container.decodeIfPresent(String.self, forKey: .caption),
                accessibilityText: try container.decode(String.self, forKey: .accessibilityText)
            )
            guard gauge.isValid else {
                throw DecodingError.dataCorruptedError(
                    forKey: .ratio,
                    in: container,
                    debugDescription: "Gauge delta data is invalid"
                )
            }
            self = .gaugeSetData(gauge)
        case .footerSetActions:
            let actions = try container.decode([UIFooterActionSpec].self, forKey: .actions)
            guard UIFooterActionsSpec(actions: actions).isValid else {
                throw DecodingError.dataCorruptedError(
                    forKey: .actions,
                    in: container,
                    debugDescription: "FooterActions delta data is invalid"
                )
            }
            self = .footerSetActions(
                nodeID: try container.decode(String.self, forKey: .nodeID),
                actions: actions
            )
        case .inputSetValue:
            self = .inputSetValue(
                nodeID: try container.decode(String.self, forKey: .nodeID),
                value: try container.decode(String.self, forKey: .value)
            )
        case .listInsertItem:
            self = .listInsertItem(
                listID: try container.decode(String.self, forKey: .listID),
                index: try container.decode(Int.self, forKey: .index),
                item: try container.decode(UIListItemSpec.self, forKey: .item)
            )
        case .listRemoveItem:
            self = .listRemoveItem(
                listID: try container.decode(String.self, forKey: .listID),
                itemID: try container.decode(String.self, forKey: .itemID)
            )
        case .listSetSelection:
            self = .listSetSelection(
                listID: try container.decode(String.self, forKey: .listID),
                selectedID: try container.decodeIfPresent(String.self, forKey: .selectedID)
            )
        case .contentSetSelection:
            self = .contentSetSelection(
                contentID: try container.decode(String.self, forKey: .contentID),
                selection: try container.decodeIfPresent(
                    UIContentSelection.self,
                    forKey: .selection
                )
            )
        case .contentSpliceLines:
            self = .contentSpliceLines(
                contentID: try container.decode(String.self, forKey: .contentID),
                index: try container.decode(Int.self, forKey: .index),
                deleteCount: try container.decode(Int.self, forKey: .deleteCount),
                lines: try container.decode([UIContentLine].self, forKey: .lines)
            )
        case .treeSetSelection:
            self = .treeSetSelection(
                nodeID: try container.decode(String.self, forKey: .nodeID),
                selectedID: try container.decodeIfPresent(String.self, forKey: .selectedID)
            )
        case .treeSetFilter:
            self = .treeSetFilter(
                filterID: try container.decode(String.self, forKey: .filterID),
                value: try container.decode(String.self, forKey: .value)
            )
        case .treeSetLocation:
            self = .treeSetLocation(
                nodeID: try container.decode(String.self, forKey: .nodeID),
                location: try container.decode(String.self, forKey: .location)
            )
        case .treeSpliceChildren:
            self = .treeSpliceChildren(
                nodeID: try container.decode(String.self, forKey: .nodeID),
                parentID: try container.decodeIfPresent(String.self, forKey: .parentID),
                index: try container.decode(Int.self, forKey: .index),
                deleteCount: try container.decode(Int.self, forKey: .deleteCount),
                items: try container.decode([UITreeItem].self, forKey: .items)
            )
        case .treeSetChildState:
            self = .treeSetChildState(
                nodeID: try container.decode(String.self, forKey: .nodeID),
                itemID: try container.decode(String.self, forKey: .itemID),
                childState: try container.decode(UITreeChildState.self, forKey: .childState)
            )
        case .treeSetExpanded:
            self = .treeSetExpanded(
                nodeID: try container.decode(String.self, forKey: .nodeID),
                itemID: try container.decode(String.self, forKey: .itemID),
                expanded: try container.decode(Bool.self, forKey: .expanded)
            )
        }
    }

    public func encode(to encoder: Encoder) throws {
        var container = encoder.container(keyedBy: CodingKeys.self)
        switch self {
        case let .replaceRoot(root):
            try container.encode(Operation.replaceRoot, forKey: .op)
            try container.encode(root, forKey: .root)
        case let .markdownReplaceRange(nodeID, edit):
            try container.encode(Operation.markdownReplaceRange, forKey: .op)
            try container.encode(nodeID, forKey: .nodeID)
            try container.encode(edit, forKey: .edit)
        case let .markdownSetSelection(nodeID, selection):
            try container.encode(Operation.markdownSetSelection, forKey: .op)
            try container.encode(nodeID, forKey: .nodeID)
            try container.encode(selection, forKey: .selection)
        case let .markdownSetPresentation(nodeID, presentation):
            try container.encode(Operation.markdownSetPresentation, forKey: .op)
            try container.encode(nodeID, forKey: .nodeID)
            try container.encode(presentation, forKey: .presentation)
        case let .markdownSetDirty(nodeID, dirty):
            try container.encode(Operation.markdownSetDirty, forKey: .op)
            try container.encode(nodeID, forKey: .nodeID)
            try container.encode(dirty, forKey: .dirty)
        case let .markdownSetReadOnly(nodeID, readOnly):
            try container.encode(Operation.markdownSetReadOnly, forKey: .op)
            try container.encode(nodeID, forKey: .nodeID)
            try container.encode(readOnly, forKey: .readOnly)
        case let .markdownSetTitle(nodeID, title):
            try container.encode(Operation.markdownSetTitle, forKey: .op)
            try container.encode(nodeID, forKey: .nodeID)
            try container.encodeIfPresent(title, forKey: .title)
            if title == nil {
                try container.encodeNil(forKey: .title)
            }
        case let .markdownSetPlaceholder(nodeID, placeholder):
            try container.encode(Operation.markdownSetPlaceholder, forKey: .op)
            try container.encode(nodeID, forKey: .nodeID)
            try container.encode(placeholder, forKey: .placeholder)
        case let .markdownSetCommandHint(nodeID, commandHint):
            try container.encode(Operation.markdownSetCommandHint, forKey: .op)
            try container.encode(nodeID, forKey: .nodeID)
            if let commandHint {
                try container.encode(commandHint, forKey: .commandHint)
            } else {
                try container.encodeNil(forKey: .commandHint)
            }
        case let .markdownSetActions(nodeID, actions):
            try container.encode(Operation.markdownSetActions, forKey: .op)
            try container.encode(nodeID, forKey: .nodeID)
            try container.encode(actions, forKey: .actions)
        case let .markdownSetMenus(nodeID, insertMenu, contextMenu):
            try container.encode(Operation.markdownSetMenus, forKey: .op)
            try container.encode(nodeID, forKey: .nodeID)
            try container.encodeIfPresent(insertMenu, forKey: .insertMenu)
            try container.encodeIfPresent(contextMenu, forKey: .contextMenu)
        case let .menuSetSelection(nodeID, selectedID):
            try container.encode(Operation.menuSetSelection, forKey: .op)
            try container.encode(nodeID, forKey: .nodeID)
            if let selectedID {
                try container.encode(selectedID, forKey: .selectedID)
            } else {
                try container.encodeNil(forKey: .selectedID)
            }
        case let .mediaSetSource(nodeID, source, intrinsic):
            try container.encode(Operation.mediaSetSource, forKey: .op)
            try container.encode(nodeID, forKey: .nodeID)
            try container.encode(source, forKey: .source)
            try container.encode(intrinsic, forKey: .intrinsic)
        case let .surfaceSetReference(nodeID, reference):
            try container.encode(Operation.surfaceSetReference, forKey: .op)
            try container.encode(nodeID, forKey: .nodeID)
            try container.encode(reference, forKey: .reference)
        case let .toggleSetValue(nodeID, value):
            try container.encode(Operation.toggleSetValue, forKey: .op)
            try container.encode(nodeID, forKey: .nodeID)
            try container.encode(value, forKey: .value)
        case let .checkmarkSetValue(nodeID, value):
            try container.encode(Operation.checkmarkSetValue, forKey: .op)
            try container.encode(nodeID, forKey: .nodeID)
            try container.encode(value, forKey: .value)
        case let .sparklineSetData(sparkline):
            try container.encode(Operation.sparklineSetData, forKey: .op)
            try container.encode(sparkline.id, forKey: .nodeID)
            try container.encode(sparkline.series, forKey: .series)
            try container.encodeIfPresent(sparkline.min, forKey: .min)
            if sparkline.min == nil { try container.encodeNil(forKey: .min) }
            try container.encodeIfPresent(sparkline.max, forKey: .max)
            if sparkline.max == nil { try container.encodeNil(forKey: .max) }
            try container.encodeIfPresent(sparkline.caption, forKey: .caption)
            if sparkline.caption == nil { try container.encodeNil(forKey: .caption) }
            try container.encodeIfPresent(sparkline.unit, forKey: .unit)
            if sparkline.unit == nil { try container.encodeNil(forKey: .unit) }
            try container.encode(sparkline.accessibilityText, forKey: .accessibilityText)
        case let .barChartSetData(chart):
            try container.encode(Operation.barChartSetData, forKey: .op)
            try container.encode(chart.id, forKey: .nodeID)
            try container.encode(chart.bars, forKey: .bars)
            try container.encode(chart.accessibilityText, forKey: .accessibilityText)
        case let .lineChartSetData(chart):
            try container.encode(Operation.lineChartSetData, forKey: .op)
            try container.encode(chart.id, forKey: .nodeID)
            try container.encode(chart.series, forKey: .series)
            try container.encode(chart.xAxis, forKey: .xAxis)
            try container.encode(chart.yAxis, forKey: .yAxis)
            try container.encode(chart.accessibilityText, forKey: .accessibilityText)
        case let .gaugeSetData(gauge):
            try container.encode(Operation.gaugeSetData, forKey: .op)
            try container.encode(gauge.id, forKey: .nodeID)
            try container.encode(gauge.ratio, forKey: .ratio)
            try container.encode(gauge.label, forKey: .label)
            try container.encodeIfPresent(gauge.caption, forKey: .caption)
            if gauge.caption == nil { try container.encodeNil(forKey: .caption) }
            try container.encode(gauge.accessibilityText, forKey: .accessibilityText)
        case let .footerSetActions(nodeID, actions):
            try container.encode(Operation.footerSetActions, forKey: .op)
            try container.encode(nodeID, forKey: .nodeID)
            try container.encode(actions, forKey: .actions)
        case let .inputSetValue(nodeID, value):
            try container.encode(Operation.inputSetValue, forKey: .op)
            try container.encode(nodeID, forKey: .nodeID)
            try container.encode(value, forKey: .value)
        case let .listInsertItem(listID, index, item):
            try container.encode(Operation.listInsertItem, forKey: .op)
            try container.encode(listID, forKey: .listID)
            try container.encode(index, forKey: .index)
            try container.encode(item, forKey: .item)
        case let .listRemoveItem(listID, itemID):
            try container.encode(Operation.listRemoveItem, forKey: .op)
            try container.encode(listID, forKey: .listID)
            try container.encode(itemID, forKey: .itemID)
        case let .listSetSelection(listID, selectedID):
            try container.encode(Operation.listSetSelection, forKey: .op)
            try container.encode(listID, forKey: .listID)
            if let selectedID {
                try container.encode(selectedID, forKey: .selectedID)
            } else {
                try container.encodeNil(forKey: .selectedID)
            }
        case let .contentSetSelection(contentID, selection):
            try container.encode(Operation.contentSetSelection, forKey: .op)
            try container.encode(contentID, forKey: .contentID)
            if let selection {
                try container.encode(selection, forKey: .selection)
            } else {
                try container.encodeNil(forKey: .selection)
            }
        case let .contentSpliceLines(contentID, index, deleteCount, lines):
            try container.encode(Operation.contentSpliceLines, forKey: .op)
            try container.encode(contentID, forKey: .contentID)
            try container.encode(index, forKey: .index)
            try container.encode(deleteCount, forKey: .deleteCount)
            try container.encode(lines, forKey: .lines)
        case let .treeSetSelection(nodeID, selectedID):
            try container.encode(Operation.treeSetSelection, forKey: .op)
            try container.encode(nodeID, forKey: .nodeID)
            if let selectedID {
                try container.encode(selectedID, forKey: .selectedID)
            } else {
                try container.encodeNil(forKey: .selectedID)
            }
        case let .treeSetFilter(filterID, value):
            try container.encode(Operation.treeSetFilter, forKey: .op)
            try container.encode(filterID, forKey: .filterID)
            try container.encode(value, forKey: .value)
        case let .treeSetLocation(nodeID, location):
            try container.encode(Operation.treeSetLocation, forKey: .op)
            try container.encode(nodeID, forKey: .nodeID)
            try container.encode(location, forKey: .location)
        case let .treeSpliceChildren(nodeID, parentID, index, deleteCount, items):
            try container.encode(Operation.treeSpliceChildren, forKey: .op)
            try container.encode(nodeID, forKey: .nodeID)
            try container.encodeIfPresent(parentID, forKey: .parentID)
            try container.encode(index, forKey: .index)
            try container.encode(deleteCount, forKey: .deleteCount)
            try container.encode(items, forKey: .items)
        case let .treeSetChildState(nodeID, itemID, childState):
            try container.encode(Operation.treeSetChildState, forKey: .op)
            try container.encode(nodeID, forKey: .nodeID)
            try container.encode(itemID, forKey: .itemID)
            try container.encode(childState, forKey: .childState)
        case let .treeSetExpanded(nodeID, itemID, expanded):
            try container.encode(Operation.treeSetExpanded, forKey: .op)
            try container.encode(nodeID, forKey: .nodeID)
            try container.encode(itemID, forKey: .itemID)
            try container.encode(expanded, forKey: .expanded)
        }
    }
}

/// Contiguous server-to-renderer change. A renderer applies it only when its
/// complete snapshot revision equals `baseRevision`.
public struct UIDelta: Codable, Equatable, Sendable {
    public let protocolName: String
    public let protocolVersion: Int
    public let appInstanceID: String
    public let clientID: String
    public let viewID: String
    public let baseRevision: Int
    public let revision: Int
    public let operations: [UIDeltaOperation]

    public init(
        protocolVersion: Int = UnpeelUIProtocol.version,
        appInstanceID: String,
        clientID: String,
        viewID: String,
        baseRevision: Int,
        revision: Int,
        operations: [UIDeltaOperation]
    ) {
        protocolName = UnpeelUIProtocol.name
        self.protocolVersion = protocolVersion
        self.appInstanceID = appInstanceID
        self.clientID = clientID
        self.viewID = viewID
        self.baseRevision = baseRevision
        self.revision = revision
        self.operations = operations
    }

    enum CodingKeys: String, CodingKey {
        case protocolName = "protocol"
        case protocolVersion
        case appInstanceID = "appInstanceId"
        case clientID = "clientId"
        case viewID = "viewId"
        case baseRevision
        case revision
        case operations
    }

    public init(from decoder: Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        protocolName = try container.decode(String.self, forKey: .protocolName)
        protocolVersion = try container.decode(Int.self, forKey: .protocolVersion)
        appInstanceID = try container.decode(String.self, forKey: .appInstanceID)
        clientID = try container.decode(String.self, forKey: .clientID)
        viewID = try container.decode(String.self, forKey: .viewID)
        baseRevision = try container.decode(Int.self, forKey: .baseRevision)
        revision = try container.decode(Int.self, forKey: .revision)
        operations = try container.decode([UIDeltaOperation].self, forKey: .operations)
        guard baseRevision >= 0,
              revision > baseRevision,
              (1...4_096).contains(operations.count)
        else {
            throw DecodingError.dataCorruptedError(
                forKey: .revision,
                in: container,
                debugDescription: "Delta must advance its base with 1...4096 operations"
            )
        }
    }
}

public struct UIDeltaApplicationError: Error, Equatable, LocalizedError, Sendable {
    public let message: String

    public init(_ message: String) {
        self.message = message
    }

    public var errorDescription: String? { message }
}

public extension UISnapshot {
    /// Applies a route-matched contiguous delta and returns complete new state.
    func applying(_ delta: UIDelta) throws -> UISnapshot {
        guard protocolName == delta.protocolName,
              protocolVersion == delta.protocolVersion,
              appInstanceID == delta.appInstanceID,
              clientID == delta.clientID,
              viewID == delta.viewID
        else {
            throw UIDeltaApplicationError("Delta route does not match the current snapshot")
        }
        guard revision == delta.baseRevision, delta.revision > delta.baseRevision else {
            throw UIDeltaApplicationError("Delta is not contiguous with the current snapshot")
        }
        guard (1...4_096).contains(delta.operations.count) else {
            throw UIDeltaApplicationError("Delta must contain 1...4096 operations")
        }

        var root = root
        for operation in delta.operations {
            root = try root.applying(operation)
        }
        if case let .markdownEditor(editor) = root.component {
            _ = try utf16Offset(for: editor.selection.anchor, in: editor.text)
            _ = try utf16Offset(for: editor.selection.head, in: editor.text)
        }
        return UISnapshot(
            protocolVersion: delta.protocolVersion,
            appInstanceID: delta.appInstanceID,
            clientID: delta.clientID,
            viewID: delta.viewID,
            revision: delta.revision,
            root: root
        )
    }
}

private extension UINode {
    func applying(_ operation: UIDeltaOperation) throws -> UINode {
        switch operation {
        case let .replaceRoot(root):
            return root
        case let .markdownReplaceRange(nodeID, edit):
            var editor = try markdownEditor(nodeID: nodeID)
            editor = editor.copying(text: try replacing(edit, in: editor.text))
            return UINode(id: id, component: .markdownEditor(editor))
        case let .markdownSetSelection(nodeID, selection):
            let editor = try markdownEditor(nodeID: nodeID).copying(selection: selection)
            return UINode(id: id, component: .markdownEditor(editor))
        case let .markdownSetPresentation(nodeID, presentation):
            let editor = try markdownEditor(nodeID: nodeID).copying(presentation: presentation)
            return UINode(id: id, component: .markdownEditor(editor))
        case let .markdownSetDirty(nodeID, dirty):
            let editor = try markdownEditor(nodeID: nodeID).copying(dirty: dirty)
            return UINode(id: id, component: .markdownEditor(editor))
        case let .markdownSetReadOnly(nodeID, readOnly):
            let editor = try markdownEditor(nodeID: nodeID).copying(readOnly: readOnly)
            return UINode(id: id, component: .markdownEditor(editor))
        case let .markdownSetTitle(nodeID, title):
            let editor = try markdownEditor(nodeID: nodeID).copying(title: .set(title))
            return UINode(id: id, component: .markdownEditor(editor))
        case let .markdownSetPlaceholder(nodeID, placeholder):
            let editor = try markdownEditor(nodeID: nodeID).copying(placeholder: placeholder)
            return UINode(id: id, component: .markdownEditor(editor))
        case let .markdownSetCommandHint(nodeID, commandHint):
            let editor = try markdownEditor(nodeID: nodeID).copying(
                commandHint: .set(commandHint)
            )
            return UINode(id: id, component: .markdownEditor(editor))
        case let .markdownSetActions(nodeID, actions):
            let editor = try markdownEditor(nodeID: nodeID).copying(actions: actions)
            return UINode(id: id, component: .markdownEditor(editor))
        case let .markdownSetMenus(nodeID, insertMenu, contextMenu):
            let editor = try markdownEditor(nodeID: nodeID).copying(
                insertMenu: .set(insertMenu),
                contextMenu: .set(contextMenu)
            )
            return UINode(id: id, component: .markdownEditor(editor))
        case let .menuSetSelection(nodeID, selectedID):
            var menu = try menu(nodeID: nodeID)
            let selected = selectedID.flatMap { id in menu.items.first { $0.id == id } }
            guard selectedID == nil || (selected != nil && selected?.disabled == false) else {
                throw UIDeltaApplicationError("Delta selects an unavailable Menu item")
            }
            menu.selectedID = selectedID
            return UINode(id: id, component: .menu(menu))
        case let .mediaSetSource(nodeID, source, intrinsic):
            let media = try media(nodeID: nodeID).copying(
                source: source,
                intrinsic: intrinsic
            )
            return UINode(id: id, component: .media(media))
        case let .surfaceSetReference(nodeID, reference):
            switch component {
            case let .surface(surface) where id == nodeID:
                return UINode(id: id, component: .surface(surface.copying(
                    reference: reference
                )))
            case let .canvasPage(page) where page.surface.id == nodeID:
                let nested = page.surface.surface.copying(reference: reference)
                return UINode(id: id, component: .canvasPage(CanvasPageSpec(
                    title: page.title,
                    surface: UICanvasSurfaceSpec(id: page.surface.id, surface: nested),
                    controls: page.controls
                )))
            default:
                throw UIDeltaApplicationError("Delta targets an unavailable Surface node")
            }
        case let .toggleSetValue(nodeID, value):
            var page = try page()
            guard case var .list(list) = page.body else {
                throw UIDeltaApplicationError("Delta targets an unavailable List")
            }
            var found = false
            for index in list.items.indices {
                var item = list.items[index]
                var itemFound = false
                item.leading = setToggle(item.leading, id: nodeID, value: value, found: &itemFound)
                item.trailing = setToggle(item.trailing, id: nodeID, value: value, found: &itemFound)
                item.accessory = setToggle(item.accessory, id: nodeID, value: value, found: &itemFound)
                if itemFound {
                    item.done = value
                    list.items[index] = item
                    found = true
                    break
                }
            }
            guard found else {
                throw UIDeltaApplicationError("Delta targets an unavailable Toggle")
            }
            page.body = .list(list)
            return UINode(id: id, component: .page(page))
        case let .checkmarkSetValue(nodeID, value):
            var page = try page()
            guard case var .list(list) = page.body else {
                throw UIDeltaApplicationError("Delta targets an unavailable List")
            }
            var found = false
            for index in list.items.indices {
                var item = list.items[index]
                var itemFound = false
                item.leading = setCheckmark(
                    item.leading,
                    id: nodeID,
                    value: value,
                    found: &itemFound
                )
                item.trailing = setCheckmark(
                    item.trailing,
                    id: nodeID,
                    value: value,
                    found: &itemFound
                )
                item.accessory = setCheckmark(
                    item.accessory,
                    id: nodeID,
                    value: value,
                    found: &itemFound
                )
                if itemFound {
                    list.items[index] = item
                    found = true
                    break
                }
            }
            guard found else {
                throw UIDeltaApplicationError("Delta targets an unavailable Checkmark")
            }
            page.body = .list(list)
            return UINode(id: id, component: .page(page))
        case let .sparklineSetData(sparkline):
            var page = try page()
            if case let .sparkline(current) = page.body, current.id == sparkline.id {
                page.body = .sparkline(UISparklineSpec(
                    id: sparkline.id,
                    series: sparkline.series,
                    min: sparkline.min,
                    max: sparkline.max,
                    caption: sparkline.caption,
                    unit: sparkline.unit,
                    accessibilityText: sparkline.accessibilityText,
                    activate: current.activate
                ))
                return UINode(id: id, component: .page(page))
            }
            guard case var .list(list) = page.body else {
                throw UIDeltaApplicationError("Delta targets an unavailable List")
            }
            var found = false
            for index in list.items.indices {
                var item = list.items[index]
                if case let .sparkline(current)? = item.trailing,
                   current.id == sparkline.id
                {
                    item.trailing = .sparkline(UISparklineSpec(
                        id: sparkline.id,
                        series: sparkline.series,
                        min: sparkline.min,
                        max: sparkline.max,
                        caption: sparkline.caption,
                        unit: sparkline.unit,
                        accessibilityText: sparkline.accessibilityText,
                        activate: current.activate
                    ))
                    list.items[index] = item
                    found = true
                    break
                }
            }
            guard found else {
                throw UIDeltaApplicationError("Delta targets an unavailable Sparkline")
            }
            page.body = .list(list)
            return UINode(id: id, component: .page(page))
        case let .barChartSetData(chart):
            var page = try page()
            guard case let .barChart(current) = page.body, current.id == chart.id else {
                throw UIDeltaApplicationError("Delta targets an unavailable BarChart")
            }
            page.body = .barChart(UIBarChartSpec(
                id: chart.id,
                bars: chart.bars,
                accessibilityText: chart.accessibilityText,
                activate: current.activate
            ))
            return UINode(id: id, component: .page(page))
        case let .lineChartSetData(chart):
            var page = try page()
            guard case let .lineChart(current) = page.body, current.id == chart.id else {
                throw UIDeltaApplicationError("Delta targets an unavailable LineChart")
            }
            page.body = .lineChart(UILineChartSpec(
                id: chart.id,
                series: chart.series,
                xAxis: chart.xAxis,
                yAxis: chart.yAxis,
                accessibilityText: chart.accessibilityText,
                activate: current.activate
            ))
            return UINode(id: id, component: .page(page))
        case let .gaugeSetData(gauge):
            var page = try page()
            if case let .gauge(current) = page.body, current.id == gauge.id {
                page.body = .gauge(UIGaugeSpec(
                    id: gauge.id,
                    ratio: gauge.ratio,
                    label: gauge.label,
                    caption: gauge.caption,
                    accessibilityText: gauge.accessibilityText,
                    activate: current.activate
                ))
                return UINode(id: id, component: .page(page))
            }
            guard case var .list(list) = page.body else {
                throw UIDeltaApplicationError("Delta targets an unavailable Gauge")
            }
            var found = false
            for index in list.items.indices {
                var item = list.items[index]
                if case let .gauge(current)? = item.trailing,
                   current.id == gauge.id
                {
                    item.trailing = .gauge(UIGaugeSpec(
                        id: gauge.id,
                        ratio: gauge.ratio,
                        label: gauge.label,
                        caption: gauge.caption,
                        accessibilityText: gauge.accessibilityText,
                        activate: current.activate
                    ))
                    list.items[index] = item
                    found = true
                    break
                }
            }
            guard found else {
                throw UIDeltaApplicationError("Delta targets an unavailable Gauge")
            }
            page.body = .list(list)
            return UINode(id: id, component: .page(page))
        case let .footerSetActions(nodeID, actions):
            guard id == nodeID else {
                throw UIDeltaApplicationError("Delta targets an unavailable footer root")
            }
            let footer = UIFooterActionsSpec(actions: actions)
            guard footer.isValid else {
                throw UIDeltaApplicationError("Delta carries invalid FooterActions")
            }
            switch component {
            case var .page(page):
                page.footer = footer
                return UINode(id: id, component: .page(page))
            case var .tree(tree):
                tree.footer = footer
                return UINode(id: id, component: .tree(tree))
            case let .markdownEditor(editor):
                return UINode(
                    id: id,
                    component: .markdownEditor(editor.copying(footer: footer))
                )
            default:
                throw UIDeltaApplicationError("Delta root has no FooterActions slot")
            }
        case let .inputSetValue(nodeID, value):
            var page = try page()
            guard case var .input(input) = page.header, input.id == nodeID else {
                throw UIDeltaApplicationError("Delta targets an unavailable Input")
            }
            input.value = value
            page.header = .input(input)
            return UINode(id: id, component: .page(page))
        case let .listInsertItem(listID, index, item):
            var page = try page()
            guard case var .list(list) = page.body,
                  list.id == listID,
                  index >= 0,
                  index <= list.items.count
            else {
                throw UIDeltaApplicationError("Delta targets an unavailable List insertion")
            }
            list.items.insert(item, at: index)
            page.body = .list(list)
            return UINode(id: id, component: .page(page))
        case let .listRemoveItem(listID, itemID):
            var page = try page()
            guard case var .list(list) = page.body,
                  list.id == listID,
                  let index = list.items.firstIndex(where: { $0.id == itemID })
            else {
                throw UIDeltaApplicationError("Delta targets an unavailable ListItem")
            }
            list.items.remove(at: index)
            page.body = .list(list)
            return UINode(id: id, component: .page(page))
        case let .listSetSelection(listID, selectedID):
            var page = try page()
            guard case var .list(list) = page.body,
                  list.id == listID,
                  selectedID.map({ selected in
                      list.items.contains(where: { $0.id == selected })
                  }) ?? true
            else {
                throw UIDeltaApplicationError("Delta targets an unavailable List selection")
            }
            list.selectedID = selectedID
            page.body = .list(list)
            return UINode(id: id, component: .page(page))
        case let .contentSetSelection(contentID, selection):
            var page = try page()
            guard case var .content(content) = page.body,
                  content.id == contentID
            else {
                throw UIDeltaApplicationError("Delta targets unavailable Content selection")
            }
            let ids = Set(content.lines.map(\.id))
            guard selection.map({ ids.contains($0.anchorID) && ids.contains($0.headID) }) ?? true
            else {
                throw UIDeltaApplicationError("Delta selects an unavailable Content line")
            }
            content.selection = selection
            page.body = .content(content)
            return UINode(id: id, component: .page(page))
        case let .contentSpliceLines(contentID, index, deleteCount, lines):
            var page = try page()
            guard case var .content(content) = page.body,
                  content.id == contentID,
                  index >= 0,
                  deleteCount >= 0,
                  index <= content.lines.count,
                  deleteCount <= content.lines.count - index
            else {
                throw UIDeltaApplicationError("Content splice is outside its collection")
            }
            content.lines.replaceSubrange(index..<(index + deleteCount), with: lines)
            page.body = .content(content)
            return UINode(id: id, component: .page(page))
        case let .treeSetSelection(nodeID, selectedID):
            var tree = try tree(nodeID: nodeID)
            let ids = Set(flattenTreeItems(tree.items).map(\.id))
            guard selectedID.map(ids.contains) ?? true else {
                throw UIDeltaApplicationError("Delta selects an unavailable Tree item")
            }
            tree.selectedID = selectedID
            return UINode(id: id, component: .tree(tree))
        case let .treeSetFilter(filterID, value):
            guard case var .tree(tree) = component,
                  var filter = tree.filter,
                  filter.id == filterID
            else {
                throw UIDeltaApplicationError("Delta targets an unavailable Tree filter")
            }
            filter.value = value
            tree.filter = filter
            return UINode(id: id, component: .tree(tree))
        case let .treeSetLocation(nodeID, location):
            var tree = try tree(nodeID: nodeID)
            tree.location = location
            return UINode(id: id, component: .tree(tree))
        case let .treeSpliceChildren(nodeID, parentID, index, deleteCount, items):
            var tree = try tree(nodeID: nodeID)
            guard index >= 0, deleteCount >= 0 else {
                throw UIDeltaApplicationError("Tree splice has a negative range")
            }
            if let parentID {
                guard spliceTreeChildren(
                    in: &tree.items,
                    parentID: parentID,
                    index: index,
                    deleteCount: deleteCount,
                    replacement: items
                ) else {
                    throw UIDeltaApplicationError("Delta targets unavailable Tree children")
                }
            } else {
                guard index <= tree.items.count,
                      deleteCount <= tree.items.count - index
                else {
                    throw UIDeltaApplicationError("Tree root splice is outside its collection")
                }
                tree.items.replaceSubrange(index..<(index + deleteCount), with: items)
            }
            guard tree.requiredCapabilities != nil else {
                throw UIDeltaApplicationError("Tree splice produced an invalid hierarchy")
            }
            return UINode(id: id, component: .tree(tree))
        case let .treeSetChildState(nodeID, itemID, childState):
            var tree = try tree(nodeID: nodeID)
            guard updateTreeItem(in: &tree.items, id: itemID, update: { item in
                item.childState = childState
                if childState != .loaded { item.children.removeAll() }
            }) else {
                throw UIDeltaApplicationError("Delta targets an unavailable Tree item")
            }
            return UINode(id: id, component: .tree(tree))
        case let .treeSetExpanded(nodeID, itemID, expanded):
            var tree = try tree(nodeID: nodeID)
            guard updateTreeItem(in: &tree.items, id: itemID, update: { item in
                item.expanded = expanded
            }), tree.requiredCapabilities != nil else {
                throw UIDeltaApplicationError("Delta targets an unavailable expandable Tree item")
            }
            return UINode(id: id, component: .tree(tree))
        }
    }

    func markdownEditor(nodeID: String) throws -> MarkdownEditorSpec {
        guard id == nodeID, case let .markdownEditor(editor) = component else {
            throw UIDeltaApplicationError("Delta targets an unavailable Markdown node")
        }
        return editor
    }

    func media(nodeID: String) throws -> MediaSpec {
        guard id == nodeID, case let .media(media) = component else {
            throw UIDeltaApplicationError("Delta targets an unavailable Media node")
        }
        return media
    }

    func menu(nodeID: String) throws -> UIMenuSpec {
        guard id == nodeID, case let .menu(menu) = component else {
            throw UIDeltaApplicationError("Delta targets an unavailable Menu node")
        }
        return menu
    }

    func page() throws -> PageSpec {
        guard case let .page(page) = component else {
            throw UIDeltaApplicationError("Delta targets an unavailable Page")
        }
        return page
    }

    func tree(nodeID: String) throws -> UITreeSpec {
        guard id == nodeID, case let .tree(tree) = component else {
            throw UIDeltaApplicationError("Delta targets an unavailable Tree node")
        }
        return tree
    }
}

private func flattenTreeItems(_ items: [UITreeItem]) -> [UITreeItem] {
    items.flatMap { item in [item] + flattenTreeItems(item.children) }
}

private func updateTreeItem(
    in items: inout [UITreeItem],
    id: String,
    update: (inout UITreeItem) -> Void
) -> Bool {
    for index in items.indices {
        if items[index].id == id {
            update(&items[index])
            return true
        }
        if updateTreeItem(in: &items[index].children, id: id, update: update) {
            return true
        }
    }
    return false
}

private func spliceTreeChildren(
    in items: inout [UITreeItem],
    parentID: String,
    index: Int,
    deleteCount: Int,
    replacement: [UITreeItem]
) -> Bool {
    for itemIndex in items.indices {
        if items[itemIndex].id == parentID {
            guard items[itemIndex].kind == .directory,
                  index <= items[itemIndex].children.count,
                  deleteCount <= items[itemIndex].children.count - index
            else { return false }
            items[itemIndex].children.replaceSubrange(
                index..<(index + deleteCount),
                with: replacement
            )
            return true
        }
        if spliceTreeChildren(
            in: &items[itemIndex].children,
            parentID: parentID,
            index: index,
            deleteCount: deleteCount,
            replacement: replacement
        ) {
            return true
        }
    }
    return false
}

private func setToggle(
    _ slot: UIListItemSlot?,
    id: String,
    value: Bool,
    found: inout Bool
) -> UIListItemSlot? {
    guard case let .toggle(toggle) = slot, toggle.id == id else { return slot }
    found = true
    return .toggle(UIToggleSpec(
        id: toggle.id,
        label: toggle.label,
        value: value,
        setValue: toggle.setValue
    ))
}

private func setCheckmark(
    _ slot: UIListItemSlot?,
    id: String,
    value: Bool,
    found: inout Bool
) -> UIListItemSlot? {
    guard case let .checkmark(checkmark) = slot, checkmark.id == id else { return slot }
    found = true
    return .checkmark(UICheckmarkSpec(
        id: checkmark.id,
        label: checkmark.label,
        value: value,
        setValue: checkmark.setValue
    ))
}

private enum OptionalStringChange {
    case unchanged
    case set(String?)
}

private enum OptionalMenuChange {
    case unchanged
    case set(UIMenuSpec?)
}

private enum OptionalCommandHintChange {
    case unchanged
    case set(MarkdownCommandHint?)
}

private extension MarkdownEditorSpec {
    func copying(
        text: String? = nil,
        selection: UITextSelection? = nil,
        presentation: MarkdownPresentation? = nil,
        readOnly: Bool? = nil,
        dirty: Bool? = nil,
        placeholder: String? = nil,
        commandHint: OptionalCommandHintChange = .unchanged,
        title: OptionalStringChange = .unchanged,
        actions: MarkdownEditorActions? = nil,
        insertMenu: OptionalMenuChange = .unchanged,
        contextMenu: OptionalMenuChange = .unchanged
        , footer: UIFooterActionsSpec? = nil
    ) -> Self {
        let nextTitle: String?
        switch title {
        case .unchanged:
            nextTitle = self.title
        case let .set(value):
            nextTitle = value
        }
        let nextInsertMenu: UIMenuSpec?
        switch insertMenu {
        case .unchanged: nextInsertMenu = self.insertMenu
        case let .set(value): nextInsertMenu = value
        }
        let nextContextMenu: UIMenuSpec?
        switch contextMenu {
        case .unchanged: nextContextMenu = self.contextMenu
        case let .set(value): nextContextMenu = value
        }
        let nextCommandHint: MarkdownCommandHint?
        switch commandHint {
        case .unchanged: nextCommandHint = self.commandHint
        case let .set(value): nextCommandHint = value
        }
        return Self(
            text: text ?? self.text,
            selection: selection ?? self.selection,
            presentation: presentation ?? self.presentation,
            readOnly: readOnly ?? self.readOnly,
            dirty: dirty ?? self.dirty,
            placeholder: placeholder ?? self.placeholder,
            commandHint: nextCommandHint,
            title: nextTitle,
            actions: actions ?? self.actions,
            insertMenu: nextInsertMenu,
            contextMenu: nextContextMenu,
            footer: footer ?? self.footer
        )
    }
}

private extension MediaSpec {
    func copying(
        source: MediaSource? = nil,
        intrinsic: MediaPixelSize? = nil
    ) -> Self {
        Self(
            source: source ?? self.source,
            intrinsic: intrinsic ?? self.intrinsic,
            cells: cells,
            points: points,
            fit: fit,
            alt: alt,
            activate: activate
        )
    }
}

private extension SurfaceSpec {
    func copying(reference: SurfaceReference) -> Self {
        Self(
            reference: reference,
            cells: cells,
            points: points,
            background: background,
            inputPolicy: inputPolicy
        )
    }
}

private func replacing(_ edit: UITextEdit, in text: String) throws -> String {
    guard edit.range.start <= edit.range.end else {
        throw UIDeltaApplicationError("Markdown text edit range is reversed")
    }
    let start = try utf16Offset(for: edit.range.start, in: text)
    let end = try utf16Offset(for: edit.range.end, in: text)
    let mutable = NSMutableString(string: text)
    mutable.replaceCharacters(in: NSRange(location: start, length: end - start), with: edit.text)
    return mutable as String
}

private func utf16Offset(for position: UITextPosition, in text: String) throws -> Int {
    guard position.line >= 0, position.utf16Column >= 0 else {
        throw UIDeltaApplicationError("Negative Markdown text position")
    }
    let units = text.utf16
    var lineStart = units.startIndex
    for _ in 0..<position.line {
        guard let newline = units[lineStart...].firstIndex(of: 10) else {
            throw UIDeltaApplicationError("Markdown text line is outside the document")
        }
        lineStart = units.index(after: newline)
    }
    let lineEnd = units[lineStart...].firstIndex(of: 10) ?? units.endIndex
    guard let target = units.index(
        lineStart,
        offsetBy: position.utf16Column,
        limitedBy: lineEnd
    ), target <= lineEnd, String.Index(target, within: text) != nil else {
        throw UIDeltaApplicationError("Markdown UTF-16 column is outside a scalar boundary")
    }
    return units.distance(from: units.startIndex, to: target)
}
