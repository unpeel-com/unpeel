import AppKit
import Charts
import SwiftUI

/// Native SwiftUI interpretation of the closed Page component family.
///
/// Page owns named header/body slots, List owns only ListItem rows, and each
/// row slot accepts only controls enumerated by `UIListItemSlot`.
@MainActor
public struct PageView: View {
    public let snapshot: UISnapshot
    public let onAction: (UIAction) -> Void

    public init(snapshot: UISnapshot, onAction: @escaping (UIAction) -> Void) {
        self.snapshot = snapshot
        self.onAction = onAction
    }

    public var body: some View {
        switch snapshot.root.component {
        case let .page(page):
            PageContent(nodeID: snapshot.root.id, page: page, onAction: onAction)
                .id(snapshot.root.id)
                .transition(.asymmetric(
                    insertion: .move(edge: .trailing).combined(with: .opacity),
                    removal: .move(edge: .leading).combined(with: .opacity)
                ))
                .animation(.snappy, value: snapshot.root.id)
        case .canvasPage, .markdownEditor, .media, .menu, .surface, .textBox, .tree, .unsupported:
            EmptyView()
        }
    }
}

@MainActor
private struct PageContent: View {
    let nodeID: String
    let page: PageSpec
    let onAction: (UIAction) -> Void
    @State private var draft = ""
    @State private var selectedID: String?
    @State private var listHeight: CGFloat = 0
    @FocusState private var listFocused: Bool
    @FocusState private var backFocused: Bool
    @State private var backHovered = false

    init(nodeID: String, page: PageSpec, onAction: @escaping (UIAction) -> Void) {
        self.nodeID = nodeID
        self.page = page
        self.onAction = onAction
        if case let .input(input) = page.header {
            _draft = State(initialValue: input.value)
        }
        if case let .list(list) = page.body {
            _selectedID = State(initialValue: list.selectedID)
        } else {
            _selectedID = State(initialValue: nil)
        }
    }

    var body: some View {
        VStack(alignment: .leading, spacing: 0) {
            HStack(spacing: 8) {
                if let back = page.back {
                    // The chevron alone carries the gray focus/hover fill,
                    // matching the terminal's back row.
                    Button {
                        onAction(UIAction(
                            nodeID: nodeID,
                            action: back,
                            kind: .cancel
                        ))
                    } label: {
                        Image(systemName: "chevron.left")
                            .font(.body.weight(.semibold))
                            .frame(width: 24, height: 22)
                            .background(
                                RoundedRectangle(cornerRadius: 6, style: .continuous)
                                    .fill(Color.primary.opacity(
                                        backFocused || backHovered ? 0.14 : 0
                                    ))
                            )
                            .contentShape(Rectangle())
                    }
                    .buttonStyle(.plain)
                    .focusable()
                    .focused($backFocused)
                    .onHover { backHovered = $0 }
                    .onKeyPress(.downArrow) {
                        backFocused = false
                        listFocused = true
                        return .handled
                    }
                    .accessibilityLabel("Back")
                }
                Text(page.title)
                    .font(.title2.weight(.semibold))
            }
                .padding(.horizontal)
                .padding(.top)
            if case let .input(input) = page.header {
                inputRow(input)
                    .padding()
                    .onChange(of: input.value) { _, value in
                        if draft != value { draft = value }
                    }
            }
            if case let .list(list) = page.body {
                if list.items.isEmpty {
                    ContentUnavailableView(
                        list.emptyMessage,
                        systemImage: "checklist"
                    )
                    .frame(maxWidth: .infinity, maxHeight: .infinity)
                } else {
                    ScrollViewReader { proxy in
                        List(list.items, selection: selectionBinding(list)) { item in
                            itemRow(item, list: list)
                                .tag(item.id)
                                .id(item.id)
                        }
                        .listStyle(.inset)
                        .focusable()
                        .focused($listFocused)
                        .onKeyPress(phases: [.down, .repeat]) { press in
                            handleKeyPress(press, list: list, proxy: proxy)
                        }
                        .background {
                            GeometryReader { geometry in
                                Color.clear
                                    .onAppear { listHeight = geometry.size.height }
                                    .onChange(of: geometry.size.height) { _, height in
                                        listHeight = height
                                    }
                            }
                        }
                        .onChange(of: list.selectedID) { _, selected in
                            guard selectedID != selected else { return }
                            selectedID = selected
                            if let selected {
                                proxy.scrollTo(selected, anchor: .center)
                            }
                        }
                    }
                }
            }
            if case let .content(content) = page.body {
                ReadOnlyContentBody(content: content, onAction: onAction)
                    .frame(maxWidth: .infinity, maxHeight: .infinity)
            }
            if case let .sparkline(sparkline) = page.body {
                ChartActivation(
                    id: sparkline.id,
                    action: sparkline.activate,
                    accessibilityText: sparkline.accessibilityText,
                    onAction: onAction
                ) {
                    NativeSparkline(
                        spec: sparkline,
                        color: .accentColor,
                        compact: false
                    )
                }
                .padding()
            }
            if case let .barChart(chart) = page.body {
                ChartActivation(
                    id: chart.id,
                    action: chart.activate,
                    accessibilityText: chart.accessibilityText,
                    onAction: onAction
                ) {
                    NativeBarChart(spec: chart)
                }
                .padding()
            }
            if case let .lineChart(chart) = page.body {
                ChartActivation(
                    id: chart.id,
                    action: chart.activate,
                    accessibilityText: chart.accessibilityText,
                    onAction: onAction
                ) {
                    NativeLineChart(spec: chart)
                }
                .padding()
            }
            if case let .gauge(gauge) = page.body {
                ChartActivation(
                    id: gauge.id,
                    action: gauge.activate,
                    accessibilityText: gauge.accessibilityText,
                    onAction: onAction
                ) {
                    NativeGauge(spec: gauge)
                }
                .padding()
            }
            FooterActionsView(footer: page.footer, onAction: onAction)
        }
    }

    private func inputRow(_ input: UIInputSpec) -> some View {
        let value = Binding(
            get: { draft },
            set: { value in
                draft = value
                guard let action = input.setValue else { return }
                onAction(UIAction(
                    nodeID: input.id,
                    action: action,
                    kind: .change,
                    value: .text(value)
                ))
            }
        )
        return HStack {
            StableInputField(
                text: value,
                placeholder: input.placeholder,
                onSubmit: { submit(input) }
            )
                .accessibilityLabel(input.label)
                .frame(minHeight: 24)
            if input.submit != nil {
                Button("Add") { submit(input) }
                    .disabled(draft.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty)
            }
        }
    }

    private func submit(_ input: UIInputSpec) {
        guard let action = input.submit else { return }
        onAction(UIAction(
            nodeID: input.id,
            action: action,
            kind: .submit,
            value: .text(draft)
        ))
        draft = ""
    }

    private func selectionBinding(_ list: UIListSpec) -> Binding<String?> {
        Binding(
            get: { selectedID },
            set: { itemID in
                guard let itemID else { return }
                select(itemID, in: list)
            }
        )
    }

    private func select(_ itemID: String, in list: UIListSpec) {
        guard list.items.contains(where: { $0.id == itemID && !$0.divider }) else { return }
        let changed = selectedID != itemID
        selectLocally(itemID, in: list)
        guard changed, let action = list.select else { return }
        onAction(UIAction(
            nodeID: list.id,
            action: action,
            kind: .change,
            value: .text(itemID)
        ))
    }

    private func selectLocally(_ itemID: String, in list: UIListSpec) {
        guard list.items.contains(where: { $0.id == itemID }) else { return }
        selectedID = itemID
    }

    private func handleKeyPress(
        _ press: KeyPress,
        list: UIListSpec,
        proxy: ScrollViewProxy
    ) -> KeyPress.Result {
        guard press.modifiers.intersection([.command, .control, .option]).isEmpty else {
            return .ignored
        }
        guard !list.items.isEmpty else { return .ignored }
        let current = list.items.firstIndex(where: { $0.id == selectedID })
            ?? list.items.firstIndex(where: { $0.id == list.selectedID })
            ?? 0
        guard let key = navigationKey(press),
              let decision = uiListNavigationDecision(
                key: key,
                primaryRole: list.items[current].primaryRole
              )
        else { return .ignored }
        if decision == .back {
            guard let back = page.back else { return .ignored }
            onAction(UIAction(nodeID: nodeID, action: back, kind: .cancel))
            return .handled
        }
        if decision == .invokePrimary {
            return invokePrimary(list.items[current], in: list) ? .handled : .ignored
        }
        if list.pageBehavior == .scroll, decision == .pageDown || decision == .pageUp {
            return .ignored
        }

        let visibleRows = max(Int(listHeight / 28), 1)
        let pageRows = max(visibleRows - max(list.pageOverlap, 0), 1)
        let last = list.items.count - 1
        let selectable = { (index: Int) in !list.items[index].divider }
        // Divider rows are passive: every move lands on the nearest real row.
        let forward = { (from: Int) in (from...last).first(where: selectable) }
        let backward = { (from: Int) in (0...from).reversed().first(where: selectable) }
        let target: Int?
        switch decision {
        case .down:
            target = forward(min(current + 1, last)) ?? (current + 1 > last ? nil : backward(current))
        case .up:
            target = backward(max(current - 1, 0)) ?? (current == 0 ? nil : forward(current))
            if page.back != nil, current == forward(0), target == nil || target == current {
                // Up from the first row focuses the back chevron.
                backFocused = true
                return .handled
            }
        case .first:
            target = forward(0)
        case .last:
            target = backward(last)
        case .pageDown:
            target = forward(min(current + pageRows, last)) ?? backward(last)
        case .pageUp:
            target = backward(max(current - pageRows, 0)) ?? forward(0)
        case .invokePrimary, .back:
            return .ignored
        }
        guard let next = target, next != current || decision == .first || decision == .last else {
            return .handled
        }
        select(list.items[next].id, in: list)
        proxy.scrollTo(list.items[next].id, anchor: list.scrollPadding > 0 ? .center : nil)
        return .handled
    }

    private func navigationKey(_ press: KeyPress) -> UIListNavigationKey? {
        switch (press.key, press.characters) {
        case (.downArrow, _), (_, "j"): .down
        case (.upArrow, _), (_, "k"): .up
        case (.home, _), (_, "g"): .first
        case (.end, _), (_, "G"): .last
        case (.pageDown, _): .pageDown
        case (.pageUp, _): .pageUp
        case (.return, _): .enter
        case (.space, _): .space
        case (.escape, _), (_, "q"): .back
        default: nil
        }
    }

    @discardableResult
    private func invokePrimary(_ item: UIListItemSpec, in list: UIListSpec) -> Bool {
        // The role action already identifies its target. Emitting selection
        // first would give both events the same base revision and make the
        // role action stale when the App advances on selection.
        selectLocally(item.id, in: list)
        switch item.primaryRole {
        case .toggle:
            guard let toggle = item.primaryToggle else { return false }
            onAction(UIAction(
                nodeID: toggle.id,
                action: toggle.setValue,
                kind: .change,
                value: .bool(!toggle.value)
            ))
        case .checkmark:
            guard let checkmark = item.primaryCheckmark else { return false }
            onAction(UIAction(
                nodeID: checkmark.id,
                action: checkmark.setValue,
                kind: .change,
                value: .bool(!checkmark.value)
            ))
        case .disclosure:
            guard let activate = item.activate else { return false }
            withAnimation(.snappy) {
                onAction(UIAction(nodeID: item.id, action: activate, kind: .activate))
            }
        case .command, .destructive:
            if let activate = item.activate {
                onAction(UIAction(nodeID: item.id, action: activate, kind: .activate))
            } else if let sparkline = item.primarySparkline,
                      let activate = sparkline.activate
            {
                onAction(UIAction(
                    nodeID: sparkline.id,
                    action: activate,
                    kind: .activate
                ))
            } else if let gauge = item.primaryGauge,
                      let activate = gauge.activate
            {
                onAction(UIAction(
                    nodeID: gauge.id,
                    action: activate,
                    kind: .activate
                ))
            } else {
                return false
            }
        case .static:
            return false
        }
        return true
    }

    private func color(for tone: UIListItemTone) -> Color {
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

    @ViewBuilder
    private func itemRow(_ item: UIListItemSpec, list: UIListSpec) -> some View {
        if item.divider {
            dividerRow(item)
        } else {
            richItemRow(item, list: list)
        }
    }

    /// A passive separator row: a thin rule with an optional muted caption.
    private func dividerRow(_ item: UIListItemSpec) -> some View {
        HStack(spacing: 8) {
            if !item.label.isEmpty {
                Text(item.label)
                    .font(.caption.weight(.medium))
                    .foregroundStyle(.secondary)
                    .lineLimit(1)
            }
            Rectangle()
                .fill(.quaternary)
                .frame(height: 1)
        }
        .padding(.vertical, 2)
        .selectionDisabled()
        .listRowSeparator(.hidden)
        .accessibilityElement(children: .combine)
        .accessibilityAddTraits(.isHeader)
    }

    /// Text rows plus optional bands, wrapped by a media column that spans
    /// every line of the item.
    private func richItemRow(_ item: UIListItemSpec, list: UIListSpec) -> some View {
        HStack(alignment: .center, spacing: 10) {
            if let media = item.media, media.side == .leading {
                ListItemMediaView(media: media, color: color(for: media.tone))
            }
            VStack(alignment: .leading, spacing: 4) {
                if let band = item.top {
                    bandView(band, item: item, list: list)
                }
                textRows(item, list: list)
                if let band = item.bottom {
                    bandView(band, item: item, list: list)
                }
            }
            if let media = item.media, media.side == .trailing {
                ListItemMediaView(media: media, color: color(for: media.tone))
            }
        }
        .contextMenu {
            if let menu = list.contextMenu {
                ForEach(menu.items) { menuItem in
                    Button(
                        menuItem.label,
                        role: menuItem.role == .danger ? .destructive : nil
                    ) {
                        selectLocally(item.id, in: list)
                        onAction(UIAction(
                            nodeID: menuItem.id,
                            action: menuItem.action,
                            kind: .activate,
                            value: .text(item.id)
                        ))
                    }
                    .disabled(menuItem.disabled)
                }
            }
        }
    }

    /// The label line with its slots. `.stacked` moves the value beneath the
    /// label; `.inline` and `.auto` keep it trailing while it fits.
    @ViewBuilder
    private func textRows(_ item: UIListItemSpec, list: UIListSpec) -> some View {
        if let value = item.value, list.rowLayout != .stacked {
            ViewThatFits(in: .horizontal) {
                itemRowContent(item, list: list, value: value)
                    .frame(minWidth: CGFloat(item.valueMinWidth ?? value.count + 11) * 8)
                itemRowContent(item, list: list, value: nil)
            }
        } else {
            itemRowContent(item, list: list, value: nil)
        }
    }

    /// A band is the whole-row interpretation of a metric. When the row also
    /// carries a trailing chart of the same kind, the band takes over here,
    /// matching the terminal.
    @ViewBuilder
    private func bandView(_ band: UIListItemBand, item: UIListItemSpec, list: UIListSpec) -> some View {
        switch band {
        case let .gauge(gauge):
            VStack(alignment: .leading, spacing: 2) {
                ProgressView(value: gauge.ratio, total: 1)
                    .progressViewStyle(.linear)
                    .tint(bandColor(for: item))
                if item.value == nil {
                    Text(gauge.valueLabel)
                        .font(.caption)
                        .foregroundStyle(.secondary)
                        .lineLimit(1)
                }
            }
            .accessibilityElement(children: .ignore)
            .accessibilityLabel(gauge.accessibilityText)
            .accessibilityValue(gauge.valueLabel)
        case let .sparkline(sparkline):
            NativeSparkline(spec: sparkline, color: bandColor(for: item), compact: false)
                .frame(height: 28)
        case let .text(text, tone):
            Text(text)
                .font(.caption)
                .foregroundStyle(color(for: tone))
                .lineLimit(1)
        case .divider:
            Rectangle()
                .fill(.quaternary)
                .frame(height: 1)
        case .unsupported:
            EmptyView()
        }
    }

    private func bandColor(for item: UIListItemSpec) -> Color {
        item.valueTone == .muted ? .accentColor : color(for: item.valueTone)
    }

    private func itemRowContent(
        _ item: UIListItemSpec,
        list: UIListSpec,
        value: String?
    ) -> some View {
        HStack {
            if item.busy {
                ProgressView()
                    .controlSize(.small)
                    .accessibilityLabel("Loading")
            }
            slot(
                item.leading,
                itemID: item.id,
                list: list,
                valueTone: item.valueTone,
                showGaugeCaption: value == nil
            )
            let stackedValue = list.rowLayout == .stacked && value == nil && item.value != nil
            if item.primaryRole == .static {
                itemLabel(item, stackedValue: stackedValue)
                    .frame(maxWidth: .infinity, alignment: .leading)
            } else {
                Button {
                    listFocused = true
                    _ = invokePrimary(item, in: list)
                } label: {
                    itemLabel(item, stackedValue: stackedValue)
                        .frame(maxWidth: .infinity, alignment: .leading)
                        .contentShape(Rectangle())
                }
                .buttonStyle(.plain)
            }
            Spacer(minLength: 4)
            if let value {
                listItemText(
                    value,
                    runs: item.valueRuns,
                    fallbackTone: item.valueTone,
                    fallbackEmphasis: .regular
                )
                    .multilineTextAlignment(.trailing)
                    .fixedSize(horizontal: true, vertical: false)
            }
            slot(
                trailingSlot(item),
                itemID: item.id,
                list: list,
                valueTone: item.valueTone,
                showGaugeCaption: value == nil
            )
            slot(
                item.accessory,
                itemID: item.id,
                list: list,
                valueTone: item.valueTone,
                showGaugeCaption: value == nil
            )
            if let action = item.delete {
                Button {
                    selectLocally(item.id, in: list)
                    onAction(UIAction(
                        nodeID: item.id,
                        action: action,
                        kind: .change
                    ))
                } label: {
                    Image(systemName: "trash")
                }
                .buttonStyle(.borderless)
                .accessibilityLabel("Delete \(item.label)")
            }
        }
    }

    private func itemLabel(_ item: UIListItemSpec, stackedValue: Bool = false) -> some View {
        VStack(alignment: .leading, spacing: 2) {
            listItemText(
                item.label,
                runs: item.labelRuns,
                fallbackTone: item.actionRole == .destructive ? .danger : item.labelTone,
                fallbackEmphasis: item.emphasis
            )
                .strikethrough(item.done)
                .opacity(item.done ? 0.68 : 1)
            if let detail = item.detail {
                listItemText(
                    detail,
                    runs: item.detailRuns,
                    fallbackTone: .muted,
                    fallbackEmphasis: .regular
                )
                    .font(.caption)
            }
            if stackedValue, let value = item.value {
                listItemText(
                    value,
                    runs: item.valueRuns,
                    fallbackTone: item.valueTone,
                    fallbackEmphasis: .regular
                )
                    .font(.caption)
            }
        }
        .contentShape(Rectangle())
    }

    /// A trailing chart slot is hidden when a band of the same kind shows the
    /// same metric across the row.
    private func trailingSlot(_ item: UIListItemSpec) -> UIListItemSlot? {
        let bands = [item.top, item.bottom].compactMap { $0 }
        switch item.trailing {
        case .gauge? where bands.contains(where: { if case .gauge = $0 { true } else { false } }):
            return nil
        case .sparkline? where bands.contains(where: { if case .sparkline = $0 { true } else { false } }):
            return nil
        default:
            return item.trailing
        }
    }

    private func listItemText(
        _ fallback: String,
        runs: [UIListItemTextRun],
        fallbackTone: UIListItemTone,
        fallbackEmphasis: UIListItemEmphasis
    ) -> Text {
        let resolved = runs.isEmpty
            ? [UIListItemTextRun(
                text: fallback,
                tone: fallbackTone,
                emphasis: fallbackEmphasis
            )]
            : runs
        return resolved.reduce(Text("")) { result, run in
            result + Text(run.text)
                .foregroundColor(color(for: run.tone ?? fallbackTone))
                .fontWeight((run.emphasis ?? fallbackEmphasis) == .strong ? .semibold : .regular)
        }
    }

    @ViewBuilder
    private func slot(
        _ slot: UIListItemSlot?,
        itemID: String,
        list: UIListSpec,
        valueTone: UIListItemTone,
        showGaugeCaption: Bool
    ) -> some View {
        switch slot {
        case let .toggle(toggle):
            Toggle(
                isOn: Binding(
                    get: { toggle.value },
                    set: { value in
                        selectLocally(itemID, in: list)
                        listFocused = true
                        onAction(UIAction(
                            nodeID: toggle.id,
                            action: toggle.setValue,
                            kind: .change,
                            value: .bool(value)
                        ))
                    }
                )
            ) {
                Text(toggle.label)
            }
            .labelsHidden()
            .accessibilityLabel(toggle.label)
            .modifier(ToggleRoleStyle(role: toggle.role))
        case let .status(status):
            Text(status.symbol)
                .foregroundStyle(color(for: status.tone))
                .fontWeight(status.emphasis == .strong ? .semibold : .regular)
                .accessibilityLabel(status.label)
        case let .badge(badge):
            Text(badge.text)
                .font(.caption.weight(.medium))
                .foregroundStyle(color(for: badge.tone))
                .padding(.horizontal, 6)
                .padding(.vertical, 2)
                .background(.quaternary, in: Capsule())
        case let .sparkline(sparkline):
            NativeSparkline(
                spec: sparkline,
                color: color(for: valueTone),
                onActivate: sparkline.activate.map { action in
                    {
                        selectLocally(itemID, in: list)
                        onAction(UIAction(
                            nodeID: sparkline.id,
                            action: action,
                            kind: .activate
                        ))
                    }
                }
            )
        case let .gauge(gauge):
            NativeCompactGauge(
                spec: gauge,
                color: color(for: valueTone),
                showCaption: showGaugeCaption,
                onActivate: gauge.activate.map { action in
                    {
                        selectLocally(itemID, in: list)
                        onAction(UIAction(
                            nodeID: gauge.id,
                            action: action,
                            kind: .activate
                        ))
                    }
                }
            )
        case .disclosure:
            Image(systemName: "chevron.right")
                .foregroundStyle(.tertiary)
                .accessibilityHidden(true)
        case let .checkmark(checkmark):
            Image(systemName: "checkmark")
                .foregroundStyle(Color.accentColor)
                .opacity(checkmark.value ? 1 : 0)
                .accessibilityLabel(checkmark.label)
                .accessibilityValue(checkmark.value ? "Selected" : "Not selected")
        case .unsupported, .none:
            EmptyView()
        }
    }
}

/// Completion toggles are checkboxes; setting toggles are switches.
private struct ToggleRoleStyle: ViewModifier {
    let role: UIToggleRole

    func body(content: Content) -> some View {
        switch role {
        case .completion:
            content.toggleStyle(.checkbox)
        case .setting:
            content.toggleStyle(.switch).controlSize(.small)
        }
    }
}

/// Media column beside a ListItem: the real image for path and inline
/// sources, otherwise a tone-colored block with the glyph.
@MainActor
private struct ListItemMediaView: View {
    let media: UIListItemMedia
    let color: Color

    @State private var image: NSImage?

    private var size: CGSize {
        CGSize(width: max(36, CGFloat(media.width) * 9), height: 36)
    }

    var body: some View {
        ZStack {
            if let image {
                Image(nsImage: image)
                    .resizable()
                    .aspectRatio(contentMode: .fill)
            } else {
                Rectangle()
                    .fill(color.opacity(0.85))
                if let glyph = media.glyph {
                    Text(glyph)
                        .font(.caption.weight(.semibold))
                        .foregroundStyle(.white)
                        .lineLimit(1)
                }
            }
        }
        .frame(width: size.width, height: size.height)
        .clipShape(RoundedRectangle(cornerRadius: 6, style: .continuous))
        .accessibilityLabel(media.spec?.alt ?? media.glyph ?? "")
        .task(id: media.spec?.source) {
            image = loadImage()
        }
    }

    private func loadImage() -> NSImage? {
        guard let spec = media.spec else { return nil }
        switch spec.source {
        case let .path(path):
            return NSImage(contentsOfFile: path)
        case let .inline(_, base64):
            guard let data = Data(base64Encoded: base64), data.count <= 262_144 else { return nil }
            return NSImage(data: data)
        case .blob:
            // Blob loading needs the Host's authorized loader; fall back to
            // the glyph block until a row-level loader is wired.
            return nil
        }
    }
}

@MainActor
private struct NativeSparkline: View {
    let spec: UISparklineSpec
    let color: Color
    var compact = true
    var onActivate: (() -> Void)?

    private struct Sample: Identifiable {
        let id: Int
        let value: Double
    }

    private var samples: [Sample] {
        spec.series.enumerated().map { Sample(id: $0.offset, value: $0.element) }
    }

    private var helpText: String {
        [spec.caption, spec.unit].compactMap { $0 }.joined(separator: " · ")
    }

    @ViewBuilder
    var body: some View {
        if let onActivate {
            Button(action: onActivate) { content }
                .buttonStyle(.plain)
        } else {
            content
        }
    }

    private var content: some View {
        VStack(alignment: .leading, spacing: 4) {
            if !compact, !helpText.isEmpty {
                Text(helpText)
                    .font(.caption)
                    .foregroundStyle(.secondary)
                    .accessibilityHidden(true)
            }
            graph
        }
        .accessibilityElement(children: .ignore)
        .accessibilityLabel(spec.accessibilityText)
        .help(helpText)
    }

    private var graph: some View {
        Chart(samples) { sample in
            LineMark(
                x: .value("Point", sample.id),
                y: .value(spec.unit ?? "Value", sample.value)
            )
            .foregroundStyle(color)
            .interpolationMethod(.linear)
            if samples.count == 1 {
                PointMark(
                    x: .value("Point", sample.id),
                    y: .value(spec.unit ?? "Value", sample.value)
                )
                .foregroundStyle(color)
                .symbolSize(12)
            }
        }
        .chartXScale(domain: 0...Swift.max(spec.series.count - 1, 1))
        .chartYScale(domain: spec.resolvedBounds)
        .chartXAxis(.hidden)
        .chartYAxis(.hidden)
        .frame(
            minWidth: compact ? Swift.min(Swift.max(CGFloat(spec.series.count) * 4, 64), 180) : 160,
            maxWidth: compact ? Swift.min(Swift.max(CGFloat(spec.series.count) * 4, 64), 180) : .infinity,
            minHeight: compact ? 24 : 120,
            maxHeight: compact ? 24 : .infinity
        )
    }
}

@MainActor
private struct ChartActivation<Content: View>: View {
    let id: String
    let action: String?
    let accessibilityText: String
    let onAction: (UIAction) -> Void
    let content: Content

    init(
        id: String,
        action: String?,
        accessibilityText: String,
        onAction: @escaping (UIAction) -> Void,
        @ViewBuilder content: () -> Content
    ) {
        self.id = id
        self.action = action
        self.accessibilityText = accessibilityText
        self.onAction = onAction
        self.content = content()
    }

    @ViewBuilder
    var body: some View {
        if let action {
            Button {
                onAction(UIAction(nodeID: id, action: action, kind: .activate))
            } label: {
                content
            }
            .buttonStyle(.plain)
            .accessibilityLabel(accessibilityText)
        } else {
            content
                .accessibilityElement(children: .contain)
                .accessibilityLabel(accessibilityText)
        }
    }
}

@MainActor
private struct NativeBarChart: View {
    let spec: UIBarChartSpec

    var body: some View {
        Chart(Array(spec.bars.enumerated()), id: \.offset) { _, bar in
            BarMark(
                x: .value("Category", bar.label),
                y: .value("Value", bar.value)
            )
            .foregroundStyle(color(for: bar.emphasis))
            .annotation(position: .top) {
                if let caption = bar.valueCaption {
                    Text(caption)
                        .font(.caption2)
                }
            }
        }
        .chartYScale(domain: 0...Swift.max(spec.bars.map(\.value).max() ?? 0, 1))
        .accessibilityElement(children: .ignore)
        .accessibilityLabel(spec.accessibilityText)
    }

    private func color(for emphasis: UIBarChartEmphasis) -> Color {
        switch emphasis {
        case .standard: .secondary
        case .accent: .accentColor
        case .danger: .red
        }
    }
}

@MainActor
private struct NativeLineChart: View {
    let spec: UILineChartSpec

    private struct Sample: Identifiable {
        let id: String
        let series: String
        let x: Double
        let y: Double
    }

    private var samples: [Sample] {
        spec.series.flatMap { series in
            series.points.enumerated().map { index, point in
                Sample(
                    id: "\(series.name)-\(index)",
                    series: series.name,
                    x: point.x,
                    y: point.y
                )
            }
        }
    }

    var body: some View {
        Chart(samples) { sample in
            LineMark(
                x: .value(spec.xAxis.label ?? "X", sample.x),
                y: .value(spec.yAxis.label ?? "Y", sample.y),
                series: .value("Series", sample.series)
            )
            .foregroundStyle(by: .value("Series", sample.series))
            .interpolationMethod(.linear)
        }
        .chartXScale(domain: spec.resolvedXBounds)
        .chartYScale(domain: spec.resolvedYBounds)
        .chartXAxisLabel(spec.xAxis.label ?? "")
        .chartYAxisLabel(spec.yAxis.label ?? "")
        .chartLegend(.visible)
        .accessibilityElement(children: .ignore)
        .accessibilityLabel(spec.accessibilityText)
    }
}

@MainActor
private struct NativeGauge: View {
    let spec: UIGaugeSpec

    var body: some View {
        SwiftUI.Gauge(value: spec.ratio, in: 0...1) {
            Text(spec.label)
        } currentValueLabel: {
            Text(spec.valueLabel)
        }
        .gaugeStyle(.accessoryLinearCapacity)
        .accessibilityElement(children: .ignore)
        .accessibilityLabel(spec.accessibilityText)
    }
}

@MainActor
private struct NativeCompactGauge: View {
    let spec: UIGaugeSpec
    let color: Color
    let showCaption: Bool
    var onActivate: (() -> Void)?

    @ViewBuilder
    var body: some View {
        if let onActivate {
            Button(action: onActivate) {
                content
            }
            .buttonStyle(.plain)
        } else {
            content
        }
    }

    private var content: some View {
        VStack(alignment: .trailing, spacing: 3) {
            if showCaption {
                Text(spec.valueLabel)
                    .font(.caption)
                    .foregroundStyle(color)
                    .lineLimit(1)
            }
            ProgressView(value: spec.ratio, total: 1)
                .progressViewStyle(.linear)
                .tint(color)
                .frame(width: 150)
        }
        .accessibilityElement(children: .ignore)
        .accessibilityLabel(spec.accessibilityText)
        .accessibilityValue(spec.valueLabel)
    }
}

/// An AppKit field whose editor survives unrelated snapshot/presence redraws.
/// SwiftUI's stock TextField can resign its field editor when a whole semantic
/// projection value is replaced, even though the Input node itself is stable.
@MainActor
private struct StableInputField: NSViewRepresentable {
    @Binding var text: String
    let placeholder: String
    let onSubmit: () -> Void

    func makeCoordinator() -> Coordinator {
        Coordinator(parent: self)
    }

    func makeNSView(context: Context) -> NSTextField {
        let field = NSTextField(string: text)
        field.delegate = context.coordinator
        field.placeholderString = placeholder
        field.isEditable = true
        field.isSelectable = true
        field.isBordered = true
        field.bezelStyle = .roundedBezel
        field.focusRingType = .default
        return field
    }

    func updateNSView(_ field: NSTextField, context: Context) {
        context.coordinator.parent = self
        field.placeholderString = placeholder
        // Never replace the active field editor underneath the user's caret.
        if field.currentEditor() == nil, field.stringValue != text {
            field.stringValue = text
        }
    }

    final class Coordinator: NSObject, NSTextFieldDelegate {
        var parent: StableInputField

        init(parent: StableInputField) {
            self.parent = parent
        }

        func controlTextDidChange(_ notification: Notification) {
            guard let field = notification.object as? NSTextField else { return }
            if parent.text != field.stringValue {
                parent.text = field.stringValue
            }
        }

        func controlTextDidEndEditing(_ notification: Notification) {
            guard let field = notification.object as? NSTextField else { return }
            if parent.text != field.stringValue {
                parent.text = field.stringValue
            }
        }

        func control(
            _ control: NSControl,
            textView _: NSTextView,
            doCommandBy commandSelector: Selector
        ) -> Bool {
            guard NSStringFromSelector(commandSelector) == "insertNewline:",
                  let field = control as? NSTextField
            else { return false }
            if parent.text != field.stringValue {
                parent.text = field.stringValue
            }
            parent.onSubmit()
            field.stringValue = parent.text
            return true
        }
    }
}
