import SwiftUI

/// Native interpretation of the closed Tree/Explorer component.
///
/// The Rust App remains the filesystem owner and router. This view sees only
/// opaque item ids and returns semantic select/open/parent/filter actions.
@MainActor
public struct TreeView: View {
    public let snapshot: UISnapshot
    public let onAction: (UIAction) -> Void

    public init(snapshot: UISnapshot, onAction: @escaping (UIAction) -> Void) {
        self.snapshot = snapshot
        self.onAction = onAction
    }

    public var body: some View {
        switch snapshot.root.component {
        case let .tree(tree):
            TreeContent(nodeID: snapshot.root.id, tree: tree, onAction: onAction)
        case .canvasPage, .markdownEditor, .media, .menu, .page, .surface, .textBox, .unsupported:
            EmptyView()
        }
    }
}

private struct VisibleTreeItem: Identifiable {
    let item: UITreeItem
    let depth: Int
    var id: String { item.id }
}

@MainActor
private struct TreeContent: View {
    let nodeID: String
    let tree: UITreeSpec
    let onAction: (UIAction) -> Void
    @State private var selectedID: String?
    @State private var filterDraft: String
    @FocusState private var filterFocused: Bool
    @FocusState private var treeFocused: Bool

    init(nodeID: String, tree: UITreeSpec, onAction: @escaping (UIAction) -> Void) {
        self.nodeID = nodeID
        self.tree = tree
        self.onAction = onAction
        _selectedID = State(initialValue: tree.selectedID)
        _filterDraft = State(initialValue: tree.filter?.value ?? "")
    }

    private var visibleItems: [VisibleTreeItem] {
        flatten(tree.items, depth: 0)
    }

    var body: some View {
        VStack(alignment: .leading, spacing: 0) {
            if let filter = tree.filter {
                TextField(filter.placeholder, text: Binding(
                    get: { filterDraft },
                    set: { value in
                        filterDraft = value
                        onAction(UIAction(
                            nodeID: filter.id,
                            action: filter.setValue,
                            kind: .change,
                            value: .text(value)
                        ))
                    }
                ))
                .textFieldStyle(.roundedBorder)
                .accessibilityLabel(filter.label)
                .focused($filterFocused)
                .padding(.horizontal)
                .padding(.top)
                .onKeyPress(phases: [.down, .repeat]) { press in
                    handleFilterKey(press)
                }
                .onChange(of: filter.value) { _, value in
                    if filterDraft != value { filterDraft = value }
                }
            }

            HStack(spacing: 6) {
                Image(systemName: "folder")
                Text(tree.location)
                    .font(.headline)
                    .lineLimit(1)
            }
            .padding(.horizontal)
            .padding(.vertical, 10)

            if let action = tree.primaryAction {
                Button(
                    action.label,
                    role: action.role == .destructive ? .destructive : nil
                ) {
                    onAction(UIAction(
                        nodeID: action.id,
                        action: action.action,
                        kind: .activate
                    ))
                }
                .buttonStyle(.borderedProminent)
                .tint(action.role == .destructive ? .red : nil)
                .padding(.horizontal)
                .padding(.bottom, 8)
            }

            if visibleItems.isEmpty {
                ContentUnavailableView(
                    tree.emptyMessage ?? "No items",
                    systemImage: "folder"
                )
                .frame(maxWidth: .infinity, maxHeight: .infinity)
            } else {
                ScrollViewReader { proxy in
                    List(visibleItems, selection: selectionBinding) { row in
                        itemRow(row)
                            .tag(row.id)
                            .id(row.id)
                    }
                    .listStyle(.inset)
                    .focusable()
                    .focused($treeFocused)
                    .onKeyPress(phases: [.down, .repeat]) { press in
                        handleKey(press, proxy: proxy)
                    }
                    .onChange(of: tree.selectedID) { _, selected in
                        guard selectedID != selected else { return }
                        selectedID = selected
                        if let selected { proxy.scrollTo(selected, anchor: .center) }
                    }
                }
            }
            FooterActionsView(footer: tree.footer, onAction: onAction)
        }
    }

    private var selectionBinding: Binding<String?> {
        Binding(
            get: { selectedID },
            set: { id in
                guard let id else { return }
                select(id)
            }
        )
    }

    private func itemRow(_ row: VisibleTreeItem) -> some View {
        HStack(spacing: 6) {
            Color.clear.frame(width: CGFloat(row.depth * 16), height: 1)
            if row.item.kind == .directory, tree.presentation == .outline {
                Button {
                    setExpanded(row.item, expanded: !row.item.expanded)
                } label: {
                    Image(systemName: row.item.expanded ? "chevron.down" : "chevron.right")
                        .frame(width: 10)
                }
                .buttonStyle(.plain)
            }
            Image(systemName: icon(row.item))
                .foregroundStyle(row.item.symlink ? .cyan : .secondary)
            Text(row.item.kind == .parent ? ".." : row.item.label)
                .lineLimit(1)
            if let detail = row.item.detail, row.item.kind != .parent {
                Text(detail)
                    .font(.caption)
                    .foregroundStyle(.secondary)
                    .lineLimit(1)
                    .truncationMode(.tail)
            }
            if row.item.childState == .loading {
                Spacer()
                ProgressView().controlSize(.small)
            }
        }
        .contentShape(Rectangle())
        .gesture(
            TapGesture(count: 2)
                .onEnded { activate(row.item) }
                .exclusively(before: TapGesture().onEnded { select(row.id) })
        )
        .contextMenu {
            if let menu = tree.contextMenu {
                ForEach(menu.items) { menuItem in
                    Button(
                        menuItem.label,
                        role: menuItem.role == .danger ? .destructive : nil
                    ) {
                        selectLocally(row.id)
                        onAction(UIAction(
                            nodeID: menuItem.id,
                            action: menuItem.action,
                            kind: .activate,
                            value: .text(row.id)
                        ))
                    }
                    .disabled(menuItem.disabled)
                }
            }
        }
        .accessibilityLabel(accessibilityLabel(row.item))
        .accessibilityAddTraits(selectedID == row.id ? .isSelected : [])
    }

    private func flatten(_ items: [UITreeItem], depth: Int) -> [VisibleTreeItem] {
        items.flatMap { item in
            var result = [VisibleTreeItem(item: item, depth: depth)]
            if tree.presentation == .outline, item.expanded {
                result += flatten(item.children, depth: depth + 1)
            }
            return result
        }
    }

    private func select(_ id: String) {
        guard visibleItems.contains(where: { $0.id == id }) else { return }
        let changed = selectedID != id
        selectLocally(id)
        guard changed else { return }
        onAction(UIAction(
            nodeID: nodeID,
            action: tree.actions.select,
            kind: .select,
            value: .text(id)
        ))
    }

    private func selectLocally(_ id: String) {
        guard visibleItems.contains(where: { $0.id == id }) else { return }
        selectedID = id
    }

    private func activate(_ item: UITreeItem) {
        selectLocally(item.id)
        if item.kind == .parent {
            parent()
        } else {
            onAction(UIAction(
                nodeID: nodeID,
                action: tree.actions.open,
                kind: .activate,
                value: .text(item.id)
            ))
        }
    }

    private func parent() {
        onAction(UIAction(
            nodeID: nodeID,
            action: tree.actions.parent,
            kind: .cancel
        ))
    }

    private func setExpanded(_ item: UITreeItem, expanded: Bool) {
        guard let action = tree.actions.setExpanded else {
            activate(item)
            return
        }
        onAction(UIAction(
            nodeID: nodeID,
            action: action,
            kind: .change,
            value: .textList([item.id, expanded ? "true" : "false"])
        ))
    }

    private func handleKey(_ press: KeyPress, proxy: ScrollViewProxy) -> KeyPress.Result {
        guard press.modifiers.intersection([.command, .control, .option]).isEmpty else {
            return .ignored
        }
        let rows = visibleItems
        guard !rows.isEmpty else { return .ignored }
        let current = rows.firstIndex(where: { $0.id == selectedID })
            ?? rows.firstIndex(where: { $0.id == tree.selectedID })
            ?? 0
        let target: Int?
        switch press.key {
        case .downArrow: target = (current + 1) % rows.count
        case .upArrow:
            if current == 0, tree.filter != nil {
                filterFocused = true
                return .handled
            }
            target = (current - 1 + rows.count) % rows.count
        case .home: target = 0
        case .end: target = rows.count - 1
        case .pageDown: target = min(current + 10, rows.count - 1)
        case .pageUp: target = max(current - 10, 0)
        case .space: target = min(current + 10, rows.count - 1)
        case .return:
            activate(rows[current].item)
            return .handled
        case .rightArrow:
            if rows[current].item.kind == .directory,
               tree.presentation == .outline,
               !rows[current].item.expanded
            {
                setExpanded(rows[current].item, expanded: true)
            } else {
                activate(rows[current].item)
            }
            return .handled
        case .leftArrow, .escape:
            parent()
            return .handled
        case .tab:
            guard tree.filter != nil else { return .ignored }
            filterFocused = true
            return .handled
        default:
            if press.characters == "/", tree.filter != nil {
                filterFocused = true
                return .handled
            }
            guard let filter = tree.filter,
                  !press.characters.isEmpty
            else { return .ignored }
            filterDraft += press.characters
            filterFocused = true
            onAction(UIAction(
                nodeID: filter.id,
                action: filter.setValue,
                kind: .change,
                value: .text(filterDraft)
            ))
            return .handled
        }
        guard let target else { return .ignored }
        select(rows[target].id)
        proxy.scrollTo(rows[target].id, anchor: .center)
        return .handled
    }

    private func handleFilterKey(_ press: KeyPress) -> KeyPress.Result {
        guard press.modifiers.intersection([.command, .control, .option]).isEmpty else {
            return .ignored
        }
        switch press.key {
        case .downArrow, .tab:
            filterFocused = false
            treeFocused = true
            return .handled
        case .escape:
            parent()
            return .handled
        default:
            return .ignored
        }
    }

    private func icon(_ item: UITreeItem) -> String {
        switch item.kind {
        case .parent: "arrow.turn.up.left"
        case .directory: item.expanded ? "folder.fill" : "folder"
        case .file: "doc"
        }
    }

    private func accessibilityLabel(_ item: UITreeItem) -> String {
        switch item.kind {
        case .parent: "Parent folder"
        case .directory: "\(item.label), folder"
        case .file: item.label
        }
    }
}
