//
//  PresetsSettingsPanel.swift
//  UnpeelNative
//
//  The Presets settings panel. Based on
//  apps/desktop/src/lib/settings/PresetsPanel.svelte, but GLOBAL-presets
//  only (the native app dropped project presets entirely):
//  - one screen: pane header and ONE grouped drag-reorderable card of
//    presets (CLI auto-detected per row; the order doubles as each CLI's
//    default choice), with inline command editing, Risky badge, favorite
//    star, add-preset row, and right-click Delete
//
//  PRESENTATION: a plain ScrollView contains one card of flat rows (grouped
//  Forms never produce `.onMove` drag-reordering on macOS, so reordering
//  remains explicit). There is no drill-in route: the list is the editor.
//
//  PERSISTENCE: all mutations go through UnpeelStore, which writes the
//  shared app-state.json preset list (or the legacy overlay before its
//  one-time migration).
//

import SwiftUI
import UniformTypeIdentifiers
import UnpeelShared

// MARK: - Drag reordering

/// Live drag-reorder for flat rows in a plain ScrollView. SwiftUI's
/// `List.onMove` is a dead end here: grouped Forms never wire the drag up on
/// macOS at all, and inside a plain List the reorder gesture handling steals
/// clicks from any text field sharing the list. Rows in a ScrollView with
/// `.onDrag`/`.onDrop` have neither problem — rows reorder live as the drag
/// passes over them, and ordinary controls keep working.
/// Six-dot grip that starts a preset reorder drag. Only this control is a
/// drag source, so the command text field keeps its own click/drag for text
/// selection.
struct PresetDragGrip: View {
    static let width: CGFloat = 12
    var prominent: Bool

    var body: some View {
        // Drawn by hand (2 × 3 dots) rather than an SF Symbol so it renders
        // identically everywhere and stays legible at this size.
        VStack(spacing: 3) {
            ForEach(0..<3, id: \.self) { _ in
                HStack(spacing: 3) {
                    Circle().frame(width: 2.5, height: 2.5)
                    Circle().frame(width: 2.5, height: 2.5)
                }
            }
        }
        .foregroundStyle(Theme.mutedForeground.opacity(prominent ? 0.95 : 0.55))
        .frame(width: Self.width, height: 20)
            .contentShape(Rectangle())
            .onHover { inside in
                if inside { NSCursor.openHand.push() } else { NSCursor.pop() }
            }
            .help("Drag to reorder")
    }
}

struct PresetReorderDropDelegate: DropDelegate {
    /// The row this delegate is attached to.
    let itemID: String
    /// Live display order while a drag is in flight. Rows shift in this
    /// projection only; nothing is persisted until the drop.
    @Binding var order: [String]
    @Binding var draggingID: String?
    /// Called once, on drop, with the final full order.
    let commit: ([String]) -> Void

    func dropEntered(info: DropInfo) {
        guard let dragging = draggingID, dragging != itemID,
              let from = order.firstIndex(of: dragging),
              let to = order.firstIndex(of: itemID)
        else { return }
        withAnimation(.easeInOut(duration: 0.15)) {
            order.move(fromOffsets: IndexSet(integer: from), toOffset: to > from ? to + 1 : to)
        }
    }

    func dropUpdated(info: DropInfo) -> DropProposal? {
        DropProposal(operation: .move)
    }

    func performDrop(info: DropInfo) -> Bool {
        if draggingID != nil { commit(order) }
        draggingID = nil
        return true
    }
}

/// Catch-all for a drag released on the list but not on a row (the add row,
/// padding, gaps): commit whatever the projection shows and clear the dim.
struct PresetReorderContainerDropDelegate: DropDelegate {
    @Binding var order: [String]
    @Binding var draggingID: String?
    let commit: ([String]) -> Void

    func dropUpdated(info: DropInfo) -> DropProposal? {
        DropProposal(operation: draggingID == nil ? .cancel : .move)
    }

    func performDrop(info: DropInfo) -> Bool {
        if draggingID != nil { commit(order) }
        draggingID = nil
        return true
    }
}

// MARK: - Panel

struct PresetsSettingsPanel: View {
    @ObservedObject var store: UnpeelStore

    @State private var newCommand = ""
    @State private var draggingPresetID: String?
    /// Display order while a drag is in flight; nil when the store is truth.
    @State private var workingOrder: [String]?

    private var presets: [Preset] {
        let list = store.mergedPresets
        guard let order = workingOrder else { return list }
        let rank = Dictionary(uniqueKeysWithValues: order.enumerated().map { ($1, $0) })
        return list.sorted { (rank[$0.id] ?? Int.max) < (rank[$1.id] ?? Int.max) }
    }

    private var orderBinding: Binding<[String]> {
        Binding(
            get: { workingOrder ?? store.mergedPresets.map(\.id) },
            set: { workingOrder = $0 }
        )
    }

    private func commitOrder(_ order: [String]) {
        store.reorderPresets(order)
        // Keep the projection until the store reloads so rows don't snap
        // back to the stale order in between.
        DispatchQueue.main.asyncAfter(deadline: .now() + 0.5) {
            if draggingPresetID == nil { workingOrder = nil }
        }
    }

    var body: some View {
        listView
            .onChange(of: store.mergedPresets.map(\.id)) { _ in
                if draggingPresetID == nil { workingOrder = nil }
            }
            // Freshen the installed-app registry AND the agent PATH scan when
            // the page opens so the add/install sections reflect a just-added
            // App or a just-installed CLI.
            .onAppear {
                store.refreshInstalledApps(force: true)
                store.refreshToolAvailability(showScanning: false)
            }
    }

    // MARK: - List view (PresetsPanel.svelte:217-313)

    /// Fixed pane header, then one grouped card containing flat preset rows
    /// and the add-command row. Reordering is `.onDrag`/`.onDrop`
    /// (`PresetReorderDropDelegate`) — deliberately NOT `List.onMove`, which
    /// grouped Forms never wire up on macOS and which steals clicks from any
    /// text field sharing the list.
    private var listView: some View {
        VStack(alignment: .leading, spacing: 0) {
            SettingsPaneHeader(
                title: "Agents & Apps",
                description: "Launch commands for your agents and installed apps. "
                    + "Edit commands here, drag the grip to reorder, and use the ⋯ menu to delete. "
                    + "A CLI's topmost preset is its default."
            )
            .padding(EdgeInsets(top: 20, leading: 20, bottom: 10, trailing: 20))

            // Interim scope labeling (scope
            // rule): while the window is scoped to another Host, launch
            // presets come from that Host's list, but this editor still writes
            // this instance's own workspace. Say so, once, right here.
            if let scopeName = store.selectedScopeDisplayName {
                HStack(alignment: .firstTextBaseline, spacing: 6) {
                    Image(systemName: "info.circle")
                        .font(.system(size: 12, weight: .medium))
                    Text("Launch presets in the current scope come from "
                        + "\(scopeName). This list belongs to "
                        + "\(UnpeelWorkspaceContext.advertisedHostName) — switch "
                        + "the workspace selector above to edit \(scopeName)'s.")
                        .font(.system(size: 13))
                        .lineSpacing(2.5)
                        .fixedSize(horizontal: false, vertical: true)
                        .frame(maxWidth: 560, alignment: .leading)
                }
                .foregroundStyle(Theme.mutedForeground)
                .padding(EdgeInsets(top: 0, leading: 20, bottom: 10, trailing: 20))
            }

            // One grouped, drag-reorderable editor. Presets are commands,
            // regardless of whether their CLI is currently on PATH.
            ScrollView {
                VStack(alignment: .leading, spacing: 18) {
                    VStack(alignment: .leading, spacing: 0) {
                        ForEach(presets) { preset in
                            presetRow(preset)
                                .opacity(draggingPresetID == preset.id ? 0.45 : 1)
                                .onDrop(of: [.text], delegate: PresetReorderDropDelegate(
                                    itemID: preset.id,
                                    order: orderBinding,
                                    draggingID: $draggingPresetID,
                                    commit: commitOrder
                                ))
                        }

                        addRow
                    }
                    .onDrop(of: [.text], delegate: PresetReorderContainerDropDelegate(
                        order: orderBinding,
                        draggingID: $draggingPresetID,
                        commit: commitOrder
                    ))
                    .background(
                        // Muted section fill like the grouped-form tabs — the old
                        // activeRow wash rendered solid white in light mode.
                        RoundedRectangle(cornerRadius: 10, style: .continuous)
                            .fill(Theme.foreground.opacity(0.06))
                    )
                    .clipShape(RoundedRectangle(cornerRadius: 10, style: .continuous))
                    .overlay(
                        RoundedRectangle(cornerRadius: 10, style: .continuous)
                            .strokeBorder(Theme.resizerLine.opacity(0.55), lineWidth: 1)
                    )

                    addableAgentsSection
                    installableAgentsSection
                    installedAppsSection
                }
                .padding(EdgeInsets(top: 0, leading: 20, bottom: 20, trailing: 20))
            }
            .frame(maxWidth: .infinity, maxHeight: .infinity)
        }
    }

    /// Inline preset editor inside the shared card. The whole row remains
    /// draggable for reordering; deletion lives in its context menu.
    private func presetRow(_ preset: Preset) -> some View {
        PresetInlineEditorRow(
            preset: preset,
            updateCommand: { command in
                store.updatePreset(id: preset.id, command: command)
            },
            toggleQuickLaunch: {
                store.updatePreset(id: preset.id, quickLaunch: !preset.quickLaunch)
            },
            delete: {
                store.removePreset(id: preset.id)
            },
            beginDrag: {
                draggingPresetID = preset.id
                workingOrder = store.mergedPresets.map(\.id)
                return NSItemProvider(object: preset.id as NSString)
            }
        )
    }

    private var addRow: some View {
        HStack(spacing: 10) {
            Color.clear.frame(width: PresetDragGrip.width, height: 1)

            ToolIconView(
                command: newCommand,
                size: 16
            )
            .foregroundStyle(
                EditorStyle.toolColor(QuickPresetTool.detect(in: newCommand))
            )
            .frame(width: 16, height: 16)
            .opacity(newCommand.trimmingCharacters(in: .whitespaces).isEmpty ? 0.45 : 1)

            TextField(
                "Add command (e.g. claude --plan)",
                text: $newCommand
            )
            .textFieldStyle(.plain)
            .font(.system(size: 13, design: .monospaced))
            .foregroundStyle(Theme.foreground)
            .onSubmit(handleAdd)

            Button("Add", action: handleAdd)
                .buttonStyle(.bordered)
                .controlSize(.small)
                .disabled(newCommand.trimmingCharacters(in: .whitespaces).isEmpty)
        }
        .padding(.vertical, 6)
        .padding(.horizontal, 12)
    }

    private func handleAdd() {
        let command = newCommand.trimmingCharacters(in: .whitespaces)
        guard !command.isEmpty else { return }
        store.addPreset(command: command)
        newCommand = ""
    }

    /// Installed Unpeel Apps not yet in the launch list, each one click away
    /// from becoming a preset — the same set the "+" new-session menu offers.
    @ViewBuilder
    private var installedAppsSection: some View {
        if !store.addableApps.isEmpty {
            VStack(alignment: .leading, spacing: 8) {
                Text("Apps you can add")
                    .font(.system(size: 12, weight: .semibold))
                    .foregroundStyle(Theme.mutedForeground)

                VStack(alignment: .leading, spacing: 0) {
                    ForEach(store.addableApps) { app in
                        installedAppRow(app)
                    }
                }
                .background(
                    RoundedRectangle(cornerRadius: 10, style: .continuous)
                        .fill(Theme.foreground.opacity(0.06))
                )
                .clipShape(RoundedRectangle(cornerRadius: 10, style: .continuous))
                .overlay(
                    RoundedRectangle(cornerRadius: 10, style: .continuous)
                        .strokeBorder(Theme.resizerLine.opacity(0.55), lineWidth: 1)
                )
            }
        }
    }

    private func installedAppRow(_ app: InstalledAppInfo) -> some View {
        HStack(spacing: 10) {
            ToolIconView(command: app.command, size: 16)
                .frame(width: 16, height: 16)

            VStack(alignment: .leading, spacing: 1) {
                Text(app.name)
                    .font(.system(size: 13))
                    .foregroundStyle(Theme.foreground)
                if !app.description.isEmpty {
                    Text(app.description)
                        .font(.system(size: 11))
                        .foregroundStyle(Theme.mutedForeground)
                        .lineLimit(1)
                        .truncationMode(.tail)
                }
            }

            Spacer(minLength: 8)

            Button("Add") { store.addAppPreset(app) }
                .buttonStyle(.bordered)
                .controlSize(.small)
        }
        .padding(.vertical, 6)
        .padding(.horizontal, 12)
    }

    // MARK: - Agents (add removed / install missing)

    /// Installed agent CLIs not in the launch list — one click re-adds them.
    @ViewBuilder
    private var addableAgentsSection: some View {
        if !store.addableAgents.isEmpty {
            agentSectionCard(title: "Agents you can add") {
                ForEach(store.addableAgents) { tool in
                    agentRow(tool) {
                        Button("Add") { store.addAgentPreset(tool) }
                            .buttonStyle(.bordered)
                            .controlSize(.small)
                    }
                }
            }
        }
    }

    /// Missing agent CLIs: Install (runs the catalog install one-liner as a
    /// visible terminal session) or Get (opens the vendor page when we have no
    /// trusted install command).
    @ViewBuilder
    private var installableAgentsSection: some View {
        if !store.installableAgents.isEmpty || !store.gettableAgents.isEmpty {
            agentSectionCard(title: "Agents you can install") {
                ForEach(store.installableAgents) { tool in
                    agentRow(tool) {
                        Button("Install") { store.installAgentSession(tool) }
                            .buttonStyle(.borderedProminent)
                            .controlSize(.small)
                    }
                }
                ForEach(store.gettableAgents) { tool in
                    agentRow(tool) {
                        Button("Get") { store.installAgentSession(tool) }
                            .buttonStyle(.bordered)
                            .controlSize(.small)
                    }
                }
            }
        }
    }

    private func agentSectionCard<Content: View>(
        title: String,
        @ViewBuilder content: () -> Content
    ) -> some View {
        VStack(alignment: .leading, spacing: 8) {
            Text(title)
                .font(.system(size: 12, weight: .semibold))
                .foregroundStyle(Theme.mutedForeground)

            VStack(alignment: .leading, spacing: 0) { content() }
                .background(
                    RoundedRectangle(cornerRadius: 10, style: .continuous)
                        .fill(Theme.foreground.opacity(0.06))
                )
                .clipShape(RoundedRectangle(cornerRadius: 10, style: .continuous))
                .overlay(
                    RoundedRectangle(cornerRadius: 10, style: .continuous)
                        .strokeBorder(Theme.resizerLine.opacity(0.55), lineWidth: 1)
                )
        }
    }

    private func agentRow<Trailing: View>(
        _ tool: SetupTool,
        @ViewBuilder trailing: () -> Trailing
    ) -> some View {
        HStack(spacing: 10) {
            ToolIconView(command: tool.defaultPresetCommand, size: 16)
                .frame(width: 16, height: 16)

            VStack(alignment: .leading, spacing: 1) {
                Text(tool.displayName)
                    .font(.system(size: 13))
                    .foregroundStyle(Theme.foreground)
                Text(tool.defaultPresetCommand)
                    .font(.system(size: 11, design: .monospaced))
                    .foregroundStyle(Theme.mutedForeground)
                    .lineLimit(1)
                    .truncationMode(.tail)
            }

            Spacer(minLength: 8)

            trailing()
        }
        .padding(.vertical, 6)
        .padding(.horizontal, 12)
    }

}

/// A preset is present or deleted; there is no disabled state in the native
/// editor. Commands save on Return or when focus leaves the field. Keeping
/// the draft local avoids rewriting app-state.json for every keystroke.
private struct PresetInlineEditorRow: View {
    let preset: Preset
    let updateCommand: (String) -> Void
    let toggleQuickLaunch: () -> Void
    let delete: () -> Void
    /// Starts a reorder drag from the row's grip. The grip is the only drag
    /// source: dragging the whole row fought the text field for the gesture,
    /// which made reordering hit-or-miss.
    let beginDrag: () -> NSItemProvider

    @State private var command: String
    @State private var hovering = false
    @FocusState private var commandFocused: Bool

    init(
        preset: Preset,
        updateCommand: @escaping (String) -> Void,
        toggleQuickLaunch: @escaping () -> Void,
        delete: @escaping () -> Void,
        beginDrag: @escaping () -> NSItemProvider
    ) {
        self.preset = preset
        self.updateCommand = updateCommand
        self.toggleQuickLaunch = toggleQuickLaunch
        self.delete = delete
        self.beginDrag = beginDrag
        _command = State(initialValue: preset.command)
    }

    private var canQuickLaunch: Bool { SetupTool.detect(in: command) != nil }

    var body: some View {
        HStack(spacing: 10) {
            PresetDragGrip(prominent: hovering)
                .onDrag(beginDrag)

            ToolIconView(command: command, size: 15)
                .foregroundStyle(
                    EditorStyle.toolColor(QuickPresetTool.detect(in: command))
                )
                .frame(width: 15, height: 15)

            TextField("Command", text: $command)
                .textFieldStyle(.plain)
                .font(.system(size: 13, design: .monospaced))
                .foregroundStyle(Theme.foreground)
                .focused($commandFocused)
                .onSubmit(commitCommand)

            if isRisky(command) {
                EditorBadge(text: "Risky", color: Theme.danger)
            }

            Spacer(minLength: 4)

            // Any known CLI can be quick-launched. Starring several presets
            // of one CLI turns its sidebar chip into a menu.
            if canQuickLaunch {
                Button(action: toggleQuickLaunch) {
                    Image(systemName: preset.quickLaunch ? "star.fill" : "star")
                        .font(.system(size: 12))
                        .foregroundStyle(preset.quickLaunch
                            ? Theme.foreground
                            : Theme.mutedForeground.opacity(0.55))
                }
                .buttonStyle(.plain)
                .help(preset.quickLaunch
                    ? "Remove from sidebar quick launch"
                    : "Add to sidebar quick launch")
            }

            Menu {
                rowActions
            } label: {
                Image(systemName: "ellipsis")
                    .font(.system(size: 12, weight: .semibold))
                    .foregroundStyle(Theme.mutedForeground.opacity(hovering ? 0.9 : 0.55))
                    .frame(width: 20, height: 20)
                    .contentShape(Rectangle())
            }
            .menuStyle(.borderlessButton)
            .menuIndicator(.hidden)
            .fixedSize()
            .help("More actions")
        }
        .padding(.vertical, 6)
        .padding(.horizontal, 12)
        .contentShape(Rectangle())
        .onHover { hovering = $0 }
        .contextMenu { rowActions }
        .onChange(of: commandFocused) { focused in
            if !focused { commitCommand() }
        }
        .onChange(of: preset.command) { savedCommand in
            if !commandFocused { command = savedCommand }
        }
    }

    /// Shared between the ⋯ button and the row's right-click menu.
    @ViewBuilder
    private var rowActions: some View {
        if canQuickLaunch {
            Button(action: toggleQuickLaunch) {
                Label(
                    preset.quickLaunch
                        ? "Remove from Quick Launch"
                        : "Add to Quick Launch",
                    systemImage: preset.quickLaunch ? "star.slash" : "star"
                )
            }
            Divider()
        }
        Button(role: .destructive, action: delete) {
            Label("Delete Preset", systemImage: "trash")
        }
    }

    private func commitCommand() {
        let trimmed = command.trimmingCharacters(in: .whitespaces)
        guard !trimmed.isEmpty else {
            command = preset.command
            return
        }
        command = trimmed
        if trimmed != preset.command {
            updateCommand(trimmed)
        }
    }

    /// Flags that hand the agent unsupervised power (permission/sandbox
    /// bypass): anything the tool author named `--dangerously…`, plus the
    /// common `--yolo` / `--force` "run everything" switches.
    private func isRisky(_ command: String) -> Bool {
        command.split(separator: " ").contains { token in
            let flag = token.lowercased()
            return flag.hasPrefix("--dangerously") || flag == "--yolo"
                || flag == "--force" || flag == "-f"
        }
    }
}

// MARK: - Styling helpers

private enum EditorStyle {
    /// Runtime descriptor tint, with a neutral fallback for a plain terminal.
    static func toolColor(_ tool: QuickPresetTool?) -> Color {
        guard let tool else { return Theme.foreground.opacity(0.88) }
        return Theme.toolColor(forCommand: tool.rawValue)
    }
}

/// Badge (.badge, PresetsPanel.svelte:431-447): 10px/600, padding 2/6,
/// radius 4, tint at 16% behind tinted text.
private struct EditorBadge: View {
    let text: String
    let color: Color

    var body: some View {
        Text(text)
            .font(.system(size: 10, weight: .semibold))
            .foregroundStyle(color)
            .padding(EdgeInsets(top: 2, leading: 6, bottom: 2, trailing: 6))
            .background(
                RoundedRectangle(cornerRadius: 4, style: .continuous)
                    .fill(color.opacity(0.16))
            )
    }
}

/// Native button (replaces the GlassButton stand-in; call sites unchanged).
/// macOS 26: primary = .glassProminent tinted Unpeel's neutral gray (the
/// designer's spec 2026-06-12: CTAs in the app gray, not system blue);
/// secondary/danger = .bordered — the standard push button, which keeps
/// secondary actions dimmer than the gray prominent CTA (`.glass` rendered
/// BRIGHTER than the tinted glassProminent over the dark vibrancy,
/// inverting the hierarchy). Danger is a destructive-role button (red
/// label). Shared with the other settings panels (SettingsView.swift).
struct EditorButton: View {
    enum Variant { case primary, secondary, danger }
    enum Size { case regular, small }

    let title: String
    var variant: Variant = .secondary
    var size: Size = .regular
    var disabled = false
    let action: () -> Void

    var body: some View {
        styled(
            Button(title, role: variant == .danger ? .destructive : nil, action: action)
        )
        .controlSize(size == .small ? .small : .regular)
        .disabled(disabled)
    }

    @ViewBuilder
    private func styled(_ button: some View) -> some View {
        if #available(macOS 26.0, *) {
            switch variant {
            // Neutral-gray prominent capsule: keep the native glass
            // material, tint it the app gray instead of system blue.
            // glassProminent derives the label color from the tint.
            case .primary: button.buttonStyle(.glassProminent).tint(Theme.ctaTint)
            case .secondary, .danger: button.buttonStyle(.bordered)
            }
        } else {
            switch variant {
            case .primary: button.buttonStyle(.borderedProminent).tint(Theme.ctaTint)
            case .secondary, .danger: button.buttonStyle(.bordered)
            }
        }
    }
}

// MARK: - Host-scoped presets (the Settings scope dropdown's first panel)

/// Reorder for the Host list: rows shuffle locally while the drag is live and
/// the single resulting index commits as ONE `settings.presets.set` effect on
/// drop — never an effect per hovered row.
/// Settings ▸ Presets while the scope dropdown targets the window's selected
/// workspace/Host (scope rule):
/// the SELECTED Host's flat preset list, read from its bootstrap snapshot and
/// edited exclusively through `RemoteHostRuntime.setPreset`
/// (`settings.presets.set`) — capability-gated, generation-bound, never a
/// local file write. The runtime refreshes its snapshot after every effect,
/// so the list reconciles to what the Host actually holds.
struct HostPresetsSettingsPanel: View {
    @ObservedObject var store: UnpeelStore
    @ObservedObject var runtime: RemoteHostRuntime

    @State private var newCommand = ""
    /// Optimistic star flips, reconciled when the refreshed snapshot lands.
    @State private var starOverrides: [String: Bool] = [:]
    @State private var draggingPresetID: String?
    /// Local row order while a drag is live; cleared once the Host's
    /// refreshed snapshot arrives (or the drag commits/cancels).
    @State private var workingOrder: [String]?
    @State private var errorMessage: String?
    @State private var busy = false

    private var hostName: String {
        store.selectedScopeDisplayName ?? "the selected Host"
    }

    private var editable: Bool {
        runtime.supportsHostOperation(RemoteHostRuntime.HostOperation.presetsSet)
    }

    private var remoteOrderBinding: Binding<[String]> {
        Binding(
            get: { workingOrder ?? presets.map(\.id) },
            set: { workingOrder = $0 }
        )
    }

    /// The Host verb moves one preset to an index, so send the dragged id's
    /// final position; the snapshot refresh reconciles the rest.
    private func commitRemoteOrder(_ order: [String]) {
        guard let dragging = draggingPresetID,
              let index = order.firstIndex(of: dragging) else { return }
        apply(RemotePresetPatch(presetID: dragging, sortOrder: index))
    }

    private var presets: [RemotePresetSummary] {
        let list = (runtime.snapshot?.presets ?? []).filter(\.enabled)
        guard let order = workingOrder else { return list }
        let byID = Dictionary(uniqueKeysWithValues: list.map { ($0.id, $0) })
        var ordered = order.compactMap { byID[$0] }
        ordered.append(contentsOf: list.filter { !order.contains($0.id) })
        return ordered
    }

    var body: some View {
        VStack(alignment: .leading, spacing: 0) {
            SettingsPaneHeader(
                title: "Agents & Apps",
                description: "Launch commands on \(hostName). Edits apply over "
                    + "the Host connection through its own preset list — the "
                    + "order is that workspace's display order everywhere."
            )
            .padding(EdgeInsets(top: 20, leading: 20, bottom: 10, trailing: 20))

            if let errorMessage {
                HStack(alignment: .firstTextBaseline, spacing: 6) {
                    Image(systemName: "exclamationmark.triangle")
                        .font(.system(size: 12, weight: .medium))
                    Text(errorMessage)
                        .font(.system(size: 13))
                        .fixedSize(horizontal: false, vertical: true)
                }
                .foregroundStyle(.red)
                .padding(EdgeInsets(top: 0, leading: 20, bottom: 10, trailing: 20))
            }

            if runtime.snapshot != nil, !editable {
                HStack(alignment: .firstTextBaseline, spacing: 6) {
                    Image(systemName: "info.circle")
                        .font(.system(size: 12, weight: .medium))
                    Text("\(hostName) is running an older Unpeel Host. Update "
                        + "Unpeel there, then reconnect to edit Agents & Apps; "
                        + "the list below stays read-only until then.")
                        .font(.system(size: 13))
                        .fixedSize(horizontal: false, vertical: true)
                        .frame(maxWidth: 560, alignment: .leading)
                }
                .foregroundStyle(Theme.mutedForeground)
                .padding(EdgeInsets(top: 0, leading: 20, bottom: 10, trailing: 20))
            }

            if runtime.snapshot == nil {
                Text("Waiting for the workspace connection…")
                    .font(.system(size: 13))
                    .foregroundStyle(Theme.mutedForeground)
                    .padding(20)
                Spacer()
            } else {
                ScrollView {
                    VStack(alignment: .leading, spacing: 18) {
                    VStack(alignment: .leading, spacing: 0) {
                        ForEach(presets) { preset in
                            presetRow(preset)
                                .opacity(draggingPresetID == preset.id ? 0.45 : 1)
                                .onDrop(of: [.text], delegate: PresetReorderDropDelegate(
                                    itemID: preset.id,
                                    order: remoteOrderBinding,
                                    draggingID: $draggingPresetID,
                                    commit: commitRemoteOrder
                                ))
                        }

                        if editable {
                            addRow
                        }
                    }
                    .onDrop(of: [.text], delegate: PresetReorderContainerDropDelegate(
                        order: remoteOrderBinding,
                        draggingID: $draggingPresetID,
                        commit: commitRemoteOrder
                    ))
                    .background(
                        RoundedRectangle(cornerRadius: 10, style: .continuous)
                            .fill(Theme.foreground.opacity(0.06))
                    )
                    .clipShape(RoundedRectangle(cornerRadius: 10, style: .continuous))
                    .overlay(
                        RoundedRectangle(cornerRadius: 10, style: .continuous)
                            .strokeBorder(Theme.resizerLine.opacity(0.55), lineWidth: 1)
                    )

                    if editable { remoteAgentsSection }
                    }
                    .padding(EdgeInsets(top: 0, leading: 20, bottom: 20, trailing: 20))
                }
                .frame(maxWidth: .infinity, maxHeight: .infinity)
            }
        }
        // A refreshed snapshot is the authoritative order — drop the local
        // drag projection so the list shows what the Host actually holds.
        .onChange(of: runtime.snapshot?.presets.map(\.id)) { _ in
            if draggingPresetID == nil {
                workingOrder = nil
            }
        }
        // Server truth arrived — drop the optimistic star layer.
        .onChange(of: runtime.snapshot?.presets) { _ in
            starOverrides = [:]
        }
    }

    /// The SAME inline editor row the local panel renders (Decision 6: one
    /// design, the scope only switches the write path).
    private func presetRow(_ summary: RemotePresetSummary) -> some View {
        let starred = starOverrides[summary.id] ?? summary.quickLaunch
        let preset = Preset(
            id: summary.id,
            label: summary.label,
            command: summary.command,
            enabled: true,
            quickLaunch: starred
        )
        return PresetInlineEditorRow(
            preset: preset,
            updateCommand: { command in
                apply(RemotePresetPatch(presetID: summary.id, command: command))
            },
            toggleQuickLaunch: {
                starOverrides[summary.id] = !starred
                apply(RemotePresetPatch(presetID: summary.id, quickLaunch: !starred))
            },
            delete: {
                apply(RemotePresetPatch(presetID: summary.id, removed: true))
            },
            beginDrag: {
                guard editable else { return NSItemProvider() }
                draggingPresetID = summary.id
                workingOrder = presets.map(\.id)
                return NSItemProvider(object: summary.id as NSString)
            }
        )
        // Recreate the row's command draft when the Host's value changes.
        .id("\(summary.id)-\(summary.command)")
        .disabled(!editable)
    }

    private var addRow: some View {
        HStack(spacing: 10) {
            Color.clear.frame(width: PresetDragGrip.width, height: 1)

            ToolIconView(
                command: newCommand,
                size: 16
            )
            .foregroundStyle(
                EditorStyle.toolColor(QuickPresetTool.detect(in: newCommand))
            )
            .frame(width: 16, height: 16)
            .opacity(newCommand.trimmingCharacters(in: .whitespaces).isEmpty ? 0.45 : 1)

            TextField(
                "Add command (e.g. claude --plan)",
                text: $newCommand
            )
            .textFieldStyle(.plain)
            .font(.system(size: 13, design: .monospaced))
            .foregroundStyle(Theme.foreground)
            .onSubmit(handleAdd)

            Button("Add", action: handleAdd)
                .buttonStyle(.bordered)
                .controlSize(.small)
                .disabled(newCommand.trimmingCharacters(in: .whitespaces).isEmpty)
        }
        .padding(.vertical, 6)
        .padding(.horizontal, 12)
    }

    private func handleAdd() {
        let command = newCommand.trimmingCharacters(in: .whitespaces)
        guard !command.isEmpty else { return }
        newCommand = ""
        apply(RemotePresetPatch(command: command))
    }

    /// One patch → one generation-bound Host effect; the runtime refreshes
    /// its snapshot afterwards, which is what updates this list.
    private func apply(_ patch: RemotePresetPatch) {
        guard editable else { return }
        busy = true
        errorMessage = nil
        Task { @MainActor in
            do {
                try await runtime.setPreset(patch)
            } catch {
                errorMessage = error.localizedDescription
            }
            busy = false
        }
    }

    // MARK: - Agents on this Host (add to list / install)

    /// Catalog agents not already in the Host's launch list. The Controller
    /// can't scan the Host's PATH, so this offers the full catalog: Add creates
    /// the preset on the Host; Install runs the setup command as a session ON
    /// the Host.
    private var remoteCatalogAgents: [SetupTool] {
        let existing = Set(
            (runtime.snapshot?.presets ?? []).compactMap { SetupTool.detect(in: $0.command) }
        )
        return SetupTool.allCases.filter { !existing.contains($0) }
    }

    /// A Host project to run an install session in (first is fine — install is
    /// project-agnostic). Nil disables Install until the Host has a project.
    private var remoteInstallProjectID: String? {
        runtime.snapshot?.projects.first?.id
    }

    /// Whether an Install button makes sense for this tool. A sibling
    /// workspace on this Mac shares this machine's PATH, so the local scan
    /// already knows the CLI is installed; a remote Host advertises no
    /// installed-agent list yet, so Install stays offered there.
    private func remoteCanInstall(_ tool: SetupTool) -> Bool {
        guard tool.installCommand != nil else { return false }
        guard store.selectedHostScope.isLocalMachine,
              let report = store.setupToolReport else { return true }
        return !report.installedStatuses.contains { $0.tool == tool }
    }

    @ViewBuilder
    private var remoteAgentsSection: some View {
        if !remoteCatalogAgents.isEmpty {
            VStack(alignment: .leading, spacing: 8) {
                Text("Agents you can add")
                    .font(.system(size: 12, weight: .semibold))
                    .foregroundStyle(Theme.mutedForeground)
                Text("Add puts the agent in \(hostName)'s launch list. Install runs "
                    + "its setup command as a session on \(hostName).")
                    .font(.system(size: 11))
                    .foregroundStyle(Theme.mutedForeground)
                    .fixedSize(horizontal: false, vertical: true)

                VStack(alignment: .leading, spacing: 0) {
                    ForEach(remoteCatalogAgents) { tool in
                        HStack(spacing: 10) {
                            ToolIconView(command: tool.defaultPresetCommand, size: 16)
                                .frame(width: 16, height: 16)

                            VStack(alignment: .leading, spacing: 1) {
                                Text(tool.displayName)
                                    .font(.system(size: 13))
                                    .foregroundStyle(Theme.foreground)
                                Text(tool.defaultPresetCommand)
                                    .font(.system(size: 11, design: .monospaced))
                                    .foregroundStyle(Theme.mutedForeground)
                                    .lineLimit(1)
                                    .truncationMode(.tail)
                            }

                            Spacer(minLength: 8)

                            if remoteCanInstall(tool), let projectID = remoteInstallProjectID {
                                Button("Install") {
                                    store.installAgentOnRemote(tool, projectID: projectID)
                                }
                                .buttonStyle(.bordered)
                                .controlSize(.small)
                            }
                            Button("Add") {
                                apply(RemotePresetPatch(command: tool.defaultPresetCommand))
                            }
                            .buttonStyle(.borderedProminent)
                            .controlSize(.small)
                        }
                        .padding(.vertical, 6)
                        .padding(.horizontal, 12)
                    }
                }
                .background(
                    RoundedRectangle(cornerRadius: 10, style: .continuous)
                        .fill(Theme.foreground.opacity(0.06))
                )
                .clipShape(RoundedRectangle(cornerRadius: 10, style: .continuous))
                .overlay(
                    RoundedRectangle(cornerRadius: 10, style: .continuous)
                        .strokeBorder(Theme.resizerLine.opacity(0.55), lineWidth: 1)
                )
            }
        }
    }
}
