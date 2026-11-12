import SwiftUI
import UnpeelShared

/// Settings ▸ Agents and Settings ▸ Plugins: one list implementation, two
/// scopes over the Host's plugin inventory. The selected Host owns inventory,
/// commands, activation, and row order (one shared `plugin_order` across both
/// pages; reordering a page moves only its rows). The Agents scope adds the
/// per-agent Unpeel integration (Install integration / Reinstall, what it edits,
/// what the agent can do with and without it) and the manual setup recipe; the Plugins
/// scope is the Unpeel Apps catalog under its user-facing name.
struct PluginSettingsPanel: View {
    enum Scope { case agents, plugins }

    @ObservedObject var store: UnpeelStore
    @ObservedObject var runtime: RemoteHostRuntime
    let scope: Scope
    @Environment(\.accessibilityReduceMotion) private var reduceMotion
    // Only the small row modifiers observe drag publications.
    @State private var drag = PluginListDragController()
    @State private var search = ""
    @State private var filter = PluginFilter.overview
    @State private var pending: [String: Int] = [:]
    @State private var activationOverrides: [String: Bool] = [:]
    @State private var quickOverrides: [String: Bool] = [:]
    @State private var orderOverride: [String]?
    @State private var drafts: [CommandDraft] = []
    @State private var customCommand = ""
    @State private var errorMessage: String?
    @State private var terminalOwner = UUID()
    @State private var installations: [Installation] = []
    @State private var selectedInstallationID: String?
    @State private var startingInstallation: String?
    @State private var updates: [String: RemotePluginUpdate] = [:]
    @State private var checkingUpdates = false
    @State private var expanded: Set<String> = []
    @State private var pendingIntegrations: Set<String> = []
    @State private var integrationErrors: [String: String] = [:]
    @State private var showManualSetup = false
    @State private var bannerDismissed = UserDefaults.standard.bool(forKey: PluginSettingsPanel.bannerDismissedKey)
    @ObservedObject private var terminalFont = TerminalFontModel.shared
    @ObservedObject private var localization = LocalizationStore.shared

    /// Per-viewer convenience only: the "install these integrations?" banner
    /// stays dismissed on this Mac; the row-level buttons never hide.
    static let bannerDismissedKey = "unpeel.native.agentsConnectBannerDismissed"

    private struct Installation: Identifiable {
        let id: String
        let title: String
        let command: String
    }
    private struct CommandDraft: Identifiable {
        let id = UUID().uuidString
        let pluginID: String
        var afterID: String?
        let command: String
        var submittedCommand: String?
        var previousCommandCount = 0
    }
    private enum PluginFilter: String, CaseIterable {
        case overview = "Overview", installed = "Installed", available = "Not Installed"

        func includes(_ item: PluginSettingsItem) -> Bool {
            switch self {
            case .overview: item.installed || item.isApp
            case .installed: item.installed
            case .available: !item.installed
            }
        }
    }
    private enum ListEntry: Identifiable {
        case header(String), item(PluginSettingsItem)
        var id: String {
            switch self { case .header(let title): "header:\(title)"; case .item(let item): item.id }
        }
    }
    private var title: String { scope == .agents ? L10n.t("Agents") : L10n.t("Plugins") }
    private var description: String {
        switch scope {
        case .agents:
            return L10n.t(
                "Install agent CLIs on this Host, install their Unpeel integrations, and choose what each one launches.")
        case .plugins:
            return L10n.t(
                "Unpeel Apps that open files and resources in a pane beside your agents. "
                    + "Install and update them here; develop your own with `unpeel apps link`.")
        }
    }
    private var searchPrompt: String {
        scope == .agents ? L10n.t("Search agents") : L10n.t("Search plugins")
    }
    private var emptyText: String {
        scope == .agents
            ? L10n.t("No agents match your search.")
            : L10n.t("No plugins match your search.")
    }
    private var canConnect: Bool {
        runtime.supportsHostOperation(RemoteHostRuntime.HostOperation.integrationsInstall)
    }
    private var motion: Animation? {
        reduceMotion ? nil : SidebarSessionDragController.slotAnimation
    }
    private var selectedInstallation: Installation? { installations.first { $0.id == selectedInstallationID } }
    private var canRunInstaller: Bool {
        runtime.supportsHostOperation(RemoteHostRuntime.HostOperation.create)
            && runtime.snapshot?.projects.isEmpty == false && startingInstallation == nil
    }
    private var canActivate: Bool { runtime.supportsHostOperation(RemoteControlProtocol.pluginsSetCapability) }
    private var canEdit: Bool { runtime.supportsHostOperation(RemoteHostRuntime.HostOperation.presetsSet) }
    private var canReorder: Bool { runtime.supportsHostOperation(RemoteControlProtocol.pluginsOrderCapability) }
    private var items: [PluginSettingsItem] {
        let source = PluginSettingsList.items(in: runtime.snapshot).filter { scope == .plugins ? $0.isApp : !$0.isApp }
        guard let orderOverride else { return source }
        let ranks = Dictionary(orderOverride.enumerated().map { ($0.element, $0.offset) }, uniquingKeysWith: min)
        return source.enumerated().sorted {
            let a = ranks[$0.element.id] ?? Int.max, b = ranks[$1.element.id] ?? Int.max
            return a == b ? $0.offset < $1.offset : a < b
        }.map(\.element)
    }
    private var visibleItems: [PluginSettingsItem] {
        items.filter { item in
            filter.includes(item)
                && (search.isEmpty || item.name.localizedCaseInsensitiveContains(search)
                    || item.command.localizedCaseInsensitiveContains(search)
                    || item.commands.contains { $0.command.localizedCaseInsensitiveContains(search) })
        }
    }
    private var activeIDs: [String] { visibleItems.filter(isActive).map(\.id) }
    private var entries: [ListEntry] {
        let visible = visibleItems
        let active = ("Active", visible.filter(isActive))
        let inactive = ("Inactive", visible.filter { $0.installed && !isActive($0) })
        let available = ("Available to install", visible.filter { !$0.installed })
        let sections: [(String, [PluginSettingsItem])] = scope == .agents
            ? [active, inactive, available]
            : [active, available, inactive]
        return sections.filter { !$0.1.isEmpty }.flatMap { title, items in
            [.header(title)] + items.map(ListEntry.item)
        }
    }
    private func isActive(_ item: PluginSettingsItem) -> Bool {
        item.installed && (activationOverrides[item.id]
            ?? runtime.snapshot?.workspaceSettings?.pluginActivation?[item.id] ?? true)
    }
    private func isPending(_ id: String) -> Bool { (pending[id] ?? 0) > 0 }
    /// Installed agent CLIs whose Unpeel integration the Host can install
    /// but the user has not yet.
    private var unconnectedAgents: [PluginSettingsItem] {
        guard scope == .agents else { return [] }
        return items.filter { $0.installed && !$0.isCustom && $0.integrationInstallable && !$0.integrationInstalled }
    }

    var body: some View {
        VStack(alignment: .leading, spacing: 0) {
            SettingsPaneHeader(title: title, description: description)
                .padding(20)
            HStack(spacing: 16) {
                HStack(spacing: 7) {
                    Image(systemName: "magnifyingglass").foregroundStyle(Theme.mutedForeground)
                    TextField(searchPrompt, text: $search).textFieldStyle(.plain)
                }
                .padding(8)
                .background(Theme.foreground.opacity(0.05), in: RoundedRectangle(cornerRadius: 8))
                HStack(spacing: 8) {
                    Text(L10n.t("Show")).fixedSize()
                    Picker(L10n.t("Show"), selection: $filter) {
                        ForEach(PluginFilter.allCases, id: \.self) { Text(L10n.t($0.rawValue)).tag($0) }
                    }.pickerStyle(.segmented).labelsHidden().frame(width: 280)
                }
            }
            .font(.system(size: 12)).padding(.horizontal, 20).padding(.bottom, 18)
            if let errorMessage {
                Label(errorMessage, systemImage: "exclamationmark.triangle")
                    .font(.system(size: 12)).foregroundStyle(Theme.danger).textSelection(.enabled)
                    .padding(.horizontal, 20).padding(.bottom, 12)
            }
            if runtime.snapshot == nil {
                ProgressView(L10n.t("Connecting to workspace…"))
                    .frame(maxWidth: .infinity, maxHeight: .infinity)
            } else {
                ScrollView {
                    VStack(alignment: .leading, spacing: 5) {
                        if !canReorder {
                            Label(
                                L10n.format("Update Unpeel on this Host to reorder %@.", title.lowercased()),
                                systemImage: "info.circle")
                                .font(.system(size: 12)).foregroundStyle(Theme.mutedForeground)
                        }
                        if !unconnectedAgents.isEmpty, canConnect, !bannerDismissed, search.isEmpty {
                            connectBanner.padding(.bottom, 8)
                        }
                        // One identity space lets a row travel between sections
                        // without destroying its command editors or draft text.
                        ForEach(entries) { entry in
                            switch entry {
                            case .header(let title):
                                Text(L10n.t(title)).font(.system(size: 13)).foregroundStyle(Theme.mutedForeground)
                                    .padding(.top, entry.id == entries.first?.id ? 0 : 12)
                                    .padding(.bottom, 5).padding(.horizontal, 4)
                            case .item(let item):
                                pluginRow(item)
                                    .background(PluginDragAnchor(controller: drag, id: item.id))
                                    .modifier(PluginRowDragEffects(controller: drag, id: item.id))
                                    .transition(.opacity)
                                    .accessibilityAction(named: L10n.t("Move up")) { moveItem(item.id, by: -1) }
                                    .accessibilityAction(named: L10n.t("Move down")) { moveItem(item.id, by: 1) }
                            }
                        }
                        if visibleItems.isEmpty {
                            Text(emptyText)
                                .font(.system(size: 13)).foregroundStyle(Theme.mutedForeground)
                        }
                        if scope == .agents, canEdit { customCommandRow.padding(.top, 8) }
                        if scope == .agents { manualSetup.padding(.top, 12) }
                        if scope == .plugins { openersRow.padding(.top, 12) }
                    }
                    .padding(.horizontal, 20).padding(.bottom, 24)
                    .background(PluginDragMonitor(controller: drag, ids: activeIDs,
                        enabled: canReorder && !isPending("order"),
                        preview: { id in
                            guard let item = items.first(where: { $0.id == id }) else { return AnyView(EmptyView()) }
                            return AnyView(pluginRow(item, preview: true))
                        }, commit: commitOrder))
                }
                .animation(motion, value: entries.map(\.id))
                .animation(motion, value: drafts.map(\.id))
                .animation(motion, value: runtime.snapshot?.presets.map(\.id))
            }
            if startingInstallation != nil || selectedInstallation != nil { installationTerminal }
        }
        .onAppear { runtime.requestImmediateRefresh(); updateTerminalPresentation() }
        .task {
            while !Task.isCancelled {
                if runtime.supportsHostOperation(RemoteControlProtocol.pluginUpdatesCapability) {
                    do {
                        let result = try await runtime.pluginUpdates()
                        try Task.checkCancellation()
                        let next = Dictionary(result.items.map { ($0.id, $0) }, uniquingKeysWith: { _, newest in newest })
                        if updates != next { updates = next }
                        checkingUpdates = result.checking
                    } catch is CancellationError { return }
                    catch { checkingUpdates = false }
                }
                do { try await Task.sleep(for: .seconds(checkingUpdates ? 1 : 5)) }
                catch { return }
            }
        }
        .onDisappear {
            drag.detach()
            runtime.setAuxiliaryTerminalSessions([], owner: terminalOwner)
        }
        .onChange(of: runtime.snapshot?.presets) { _ in
            // Editing a generated default gives it a durable preset ID. Keep
            // an open variant editor attached across that identity change.
            drafts.removeAll { draft in
                guard let value = draft.submittedCommand else { return false }
                let count = (runtime.snapshot?.presets ?? []).filter { $0.projectID == nil && $0.command == value }.count
                return count > draft.previousCommandCount
            }
            for (id, value) in quickOverrides {
                let commands = items.first(where: { $0.id == id })?.commands ?? []
                if commands.allSatisfy({ $0.quickLaunch == value }) {
                    quickOverrides.removeValue(forKey: id)
                }
            }
            for index in drafts.indices {
                guard let item = items.first(where: { $0.id == drafts[index].pluginID }),
                      !item.commands.contains(where: { $0.id == drafts[index].afterID }) else { continue }
                drafts[index].afterID = item.commands.first?.id
            }
        }
        .onChange(of: selectedInstallationID) { _ in updateTerminalPresentation() }
        .onChange(of: runtime.snapshot?.sessions.map(\.id)) { _ in updateTerminalPresentation() }
        .onChange(of: runtime.snapshot?.workspaceSettings?.pluginActivation) { values in
            for (id, value) in activationOverrides where (values?[id] ?? true) == value {
                activationOverrides.removeValue(forKey: id)
            }
        }
        .onChange(of: runtime.snapshot?.workspaceSettings?.pluginOrder) { value in
            orderOverride = nil
        }
        .task {
            let environment = ProcessInfo.processInfo.environment
            guard environment["UNPEEL_SNAPSHOT"] != nil,
                  let command = environment["UNPEEL_TEST_SETTINGS_COMMAND"], !command.isEmpty else { return }
            for _ in 0..<100 {
                if canRunInstaller {
                    runInstaller(id: "snapshot", title: L10n.t("Installation terminal"), command: command)
                    return
                }
                do { try await Task.sleep(nanoseconds: 100_000_000) } catch { return }
            }
        }
    }

    private func pluginRow(_ item: PluginSettingsItem, preview: Bool = false) -> some View {
        VStack(alignment: .leading, spacing: 0) {
            rowContent(item, preview: preview)
            if !preview, hasDetails(item), expanded.contains(item.id) {
                Rectangle().fill(Theme.resizerLine.opacity(0.55)).frame(height: 1).padding(.top, 6)
                agentDetails(item)
            }
        }
        .padding(.horizontal, 10).padding(.vertical, 6)
        .background(Theme.foreground.opacity(0.035), in: RoundedRectangle(cornerRadius: 7))
        .overlay(RoundedRectangle(cornerRadius: 7).strokeBorder(Theme.resizerLine.opacity(0.55), lineWidth: 1))
        .allowsHitTesting(!preview)
    }

    /// Agent rows expand to their integration details; plugins and custom
    /// commands have none.
    private func hasDetails(_ item: PluginSettingsItem) -> Bool {
        scope == .agents && item.installed && !item.isCustom
    }
    private func toggleExpanded(_ id: String) {
        withAnimation(motion) {
            if expanded.contains(id) { expanded.remove(id) } else { expanded.insert(id) }
        }
    }

    private func rowContent(_ item: PluginSettingsItem, preview: Bool) -> some View {
        HStack(alignment: .top, spacing: 12) {
            ToolIconView(appID: item.appID, command: item.command, size: 19)
                .frame(width: 22, height: 24)
                .help(item.name)
                .accessibilityLabel(item.name)
            VStack(alignment: .leading, spacing: 2) {
                if item.commands.isEmpty {
                    Text(item.command).font(.system(size: 13)).foregroundStyle(Theme.mutedForeground)
                        .lineLimit(1).textSelection(.enabled)
                        .frame(height: 22)
                }
                ForEach(item.commands) { command in
                    commandEditor(item: item, command: command, preview: preview)
                    ForEach(drafts.filter { $0.pluginID == item.id && $0.afterID == command.id }) { draft in
                        draftEditor(draft, item: item, preview: preview)
                    }
                }
            }
            .frame(maxWidth: .infinity, minHeight: 24, alignment: .leading)
            if let version = item.displayVersion {
                Text(version.hasPrefix("v") ? version : "v\(version)")
                    .font(.system(size: 11)).monospacedDigit()
                    .foregroundStyle(Theme.mutedForeground)
                    .fixedSize().frame(height: 24)
                    .help(item.installed ? L10n.t("Installed version") : L10n.t("Available version"))
            }
            HStack(spacing: 10) {
                integrationControl(item)
                installationControl(item).frame(width: 64, height: 24, alignment: .trailing)
                if item.installed {
                    Group {
                        if !item.commands.isEmpty { quickButton(item) }
                        else { Color.clear }
                    }.frame(width: 22, height: 24)
                    Toggle(L10n.format("Activate %@", item.name), isOn: Binding(get: { isActive(item) }, set: { value in
                        withAnimation(motion) { activationOverrides[item.id] = value }
                        perform(id: item.id) {
                            do { try await runtime.setPluginActive(id: item.id, active: value) }
                            catch { withAnimation(motion) { _ = activationOverrides.removeValue(forKey: item.id) }; throw error }
                        }
                    }))
                    .labelsHidden().toggleStyle(.switch).controlSize(.mini)
                    .disabled(!canActivate || isPending(item.id))
                    .background(PluginDragExclusion(controller: drag))
                    .frame(width: 32, height: 24)
                }
                if hasDetails(item) {
                    Button { toggleExpanded(item.id) } label: {
                        Image(systemName: expanded.contains(item.id) ? "chevron.down" : "chevron.right")
                            .font(.system(size: 10, weight: .semibold))
                            .frame(width: 18, height: 24)
                    }
                    .buttonStyle(.plain).foregroundStyle(Theme.mutedForeground)
                    .accessibilityLabel(
                        expanded.contains(item.id)
                            ? L10n.format("Hide %@ details", item.name)
                            : L10n.format("Show %@ details", item.name))
                    .help(
                        expanded.contains(item.id)
                            ? L10n.t("Hide details")
                            : L10n.t("Integration, capabilities, and manual setup"))
                    .background(PluginDragExclusion(controller: drag))
                }
            }
        }
    }

    // MARK: Unpeel integration (Agents scope)

    /// The row-level integration state. Installing runs the Host's
    /// `integrations.install` verb: hooks + the unpeel MCP server registered
    /// in the CLI's own configuration, once per Host. The Controller knows
    /// nothing about provider file formats; a remote Host renders identically.
    @ViewBuilder
    private func integrationControl(_ item: PluginSettingsItem) -> some View {
        if scope == .agents, item.installed, !item.isCustom, item.integrationInstallable {
            if pendingIntegrations.contains(item.id) {
                ProgressView().controlSize(.small).frame(width: 22, height: 24)
            } else if item.integrationInstalled {
                Label(L10n.t("Integration installed"), systemImage: "checkmark.circle")
                    .labelStyle(.iconOnly)
                    .font(.system(size: 12))
                    .foregroundStyle(Theme.mutedForeground)
                    .frame(width: 22, height: 24)
                    .help(L10n.format(
                        "Integration installed: Unpeel's hooks and MCP server are registered in %@'s own config.",
                        item.name))
            } else {
                Button(L10n.t("Install integration")) { connect(item) }
                    .buttonStyle(.bordered).controlSize(.small)
                    .disabled(!canConnect)
                    .help(canConnect
                        ? L10n.format("Register Unpeel's hooks and MCP server in %@'s own config", item.name)
                        : L10n.t("This Host does not support installing integrations"))
                    .background(PluginDragExclusion(controller: drag))
            }
        }
    }

    private func connect(_ item: PluginSettingsItem) {
        pendingIntegrations.insert(item.id)
        integrationErrors.removeValue(forKey: item.id)
        Task { @MainActor in
            defer { pendingIntegrations.remove(item.id) }
            do {
                try await runtime.installIntegration(runtimeID: item.id)
                runtime.requestImmediateRefresh()
            } catch {
                integrationErrors[item.id] = error.localizedDescription
                withAnimation(motion) { expanded.insert(item.id) }
            }
        }
    }

    private func connectAll() {
        for item in unconnectedAgents where !pendingIntegrations.contains(item.id) { connect(item) }
    }

    private func dismissBanner() {
        withAnimation(motion) { bannerDismissed = true }
        UserDefaults.standard.set(true, forKey: Self.bannerDismissedKey)
    }

    /// First-run nudge: agents were found on the Host that Unpeel could
    /// integrate. One click installs them all; each integration edits that
    /// agent's own global configuration once.
    private var connectBanner: some View {
        let count = unconnectedAgents.count
        return HStack(alignment: .top, spacing: 12) {
            Image(systemName: "sparkles").font(.system(size: 14)).foregroundStyle(Color.accentColor)
                .frame(width: 22, height: 24)
            VStack(alignment: .leading, spacing: 2) {
                Text(count == 1
                    ? L10n.format(
                        "%@ is installed without its Unpeel integration",
                        unconnectedAgents[0].name)
                    : L10n.format("%ld agents are installed without their Unpeel integration", count))
                    .font(.system(size: 12, weight: .semibold))
                Text(L10n.t(
                    "Install it for exact busy and idle, done notifications, transcripts, precise resume, "
                        + "and Unpeel's tools inside each agent. Each integration registers Unpeel's hooks and "
                        + "MCP server in that agent's own configuration once."))
                    .font(.system(size: 11)).foregroundStyle(Theme.mutedForeground)
                    .fixedSize(horizontal: false, vertical: true)
            }
            Spacer(minLength: 8)
            Button(count == 1 ? L10n.t("Install integration") : L10n.t("Install all")) { connectAll() }
                .buttonStyle(.borderedProminent).controlSize(.small)
                .disabled(!pendingIntegrations.isEmpty)
            Button { dismissBanner() } label: {
                Image(systemName: "xmark").font(.system(size: 10, weight: .semibold)).frame(width: 18, height: 24)
            }
            .buttonStyle(.plain).foregroundStyle(Theme.mutedForeground)
            .accessibilityLabel(L10n.t("Dismiss"))
        }
        .padding(.horizontal, 12).padding(.vertical, 10)
        .background(Color.accentColor.opacity(0.08), in: RoundedRectangle(cornerRadius: 8))
        .overlay(RoundedRectangle(cornerRadius: 8).strokeBorder(Color.accentColor.opacity(0.25), lineWidth: 1))
    }

    /// The expanded agent row: integration status, what installing edits (copy
    /// from the runtime package), what the agent can do with and without it,
    /// and the provider's own MCP registration command for hand setup.
    private func agentDetails(_ item: PluginSettingsItem) -> some View {
        let meta = UnpeelRuntimeCatalog.runtime(id: item.id)
        return VStack(alignment: .leading, spacing: 8) {
            HStack(alignment: .top, spacing: 12) {
                VStack(alignment: .leading, spacing: 3) {
                    Text(integrationTitle(item)).font(.system(size: 12, weight: .semibold))
                    Text(integrationSummary(item))
                        .font(.system(size: 11)).foregroundStyle(Theme.mutedForeground)
                        .fixedSize(horizontal: false, vertical: true)
                    Text(capabilitiesLine(item, meta: meta))
                        .font(.system(size: 11)).foregroundStyle(Theme.mutedForeground)
                        .fixedSize(horizontal: false, vertical: true)
                }
                Spacer(minLength: 8)
                if item.integrationInstallable {
                    if pendingIntegrations.contains(item.id) {
                        ProgressView().controlSize(.small)
                    } else if item.integrationInstalled {
                        Button(L10n.t("Reinstall")) { connect(item) }
                            .buttonStyle(.plain).font(.system(size: 11))
                            .foregroundStyle(Theme.mutedForeground)
                            .disabled(!canConnect)
                            .help(L10n.t("Rewrite the hook script and MCP registration for this Host build"))
                    } else {
                        Button(L10n.t("Install integration")) { connect(item) }
                            .buttonStyle(.bordered).controlSize(.small)
                            .disabled(!canConnect)
                    }
                }
            }
            if let manual = item.integrationManualCommand, !manual.isEmpty {
                VStack(alignment: .leading, spacing: 2) {
                    Text(L10n.t("MCP only, by hand (no hooks):"))
                        .font(.system(size: 11)).foregroundStyle(Theme.mutedForeground)
                    Text(manual)
                        .font(.system(size: 11, design: .monospaced))
                        .textSelection(.enabled)
                }
            }
            if let error = integrationErrors[item.id] {
                Text(error).font(.system(size: 11)).foregroundStyle(Theme.danger)
                    .textSelection(.enabled).fixedSize(horizontal: false, vertical: true)
            }
        }
        .padding(.top, 8).padding(.bottom, 2)
        .background(PluginDragExclusion(controller: drag))
    }

    private func integrationTitle(_ item: PluginSettingsItem) -> String {
        guard item.integrationInstallable else { return L10n.t("No Unpeel integration") }
        return item.integrationInstalled
            ? L10n.t("Unpeel integration installed")
            : L10n.t("Unpeel integration not installed")
    }
    private func integrationSummary(_ item: PluginSettingsItem) -> String {
        guard item.integrationInstallable else {
            return L10n.format(
                "%@ has no hooks or MCP configuration Unpeel can register; Unpeel still recognizes it by name.",
                item.name)
        }
        return item.integrationSummary
            ?? L10n.format(
                "Registers Unpeel's hooks and the unpeel MCP server in %@'s own configuration.",
                item.name)
    }
    /// Capability honesty per state: hooks are the busy/idle authority, the
    /// screen tier is the declared fallback, detection alone grants identity.
    private func capabilitiesLine(_ item: PluginSettingsItem, meta: UnpeelRuntimeMetadata?) -> String {
        if item.integrationInstallable && item.integrationInstalled {
            return L10n.format(
                "Exact busy and idle from hooks, done notifications, transcripts, precise resume, "
                    + "and Unpeel's tools inside %@.",
                item.name)
        }
        if item.integrationInstallable {
            return meta?.lifecycleFallback == "screen"
                ? L10n.t(
                    "Without it: busy and idle are read from the screen; no done notifications, "
                        + "transcripts, precise resume, or Unpeel tools.")
                : L10n.t(
                    "Without it: identity and tint only; no busy and idle, done notifications, "
                        + "transcripts, precise resume, or Unpeel tools.")
        }
        return meta?.lifecycleFallback == "screen"
            ? L10n.format(
                "Identity and tint from detection; busy and idle from the screen; Resume uses %@'s own continue-last.",
                item.name)
            : L10n.format(
                "Identity and tint from detection; Resume uses %@'s own continue-last.",
                item.name)
    }

    /// The transparent alternative at the bottom of the Agents page: the one
    /// shim every integration registers, and the CLI verb. Per-agent
    /// commands sit in each row's details.
    private var manualSetup: some View {
        DisclosureGroup(L10n.t("Manual setup"), isExpanded: $showManualSetup) {
            VStack(alignment: .leading, spacing: 6) {
                Text(L10n.t(
                    "Every integration points the agent's MCP config at one launcher on this Host. "
                        + "Register it with an agent's own command (in the agent's details above) to "
                        + "get Unpeel's tools without the hooks; Install integration does both."))
                    .font(.system(size: 11)).foregroundStyle(Theme.mutedForeground)
                    .fixedSize(horizontal: false, vertical: true)
                if let shim = runtime.snapshot?.workspaceSettings?.mcpShimPath {
                    Text(shim).font(.system(size: 11, design: .monospaced)).textSelection(.enabled)
                }
                Text(L10n.t("Or from any terminal: unpeel integrations install <agent>"))
                    .font(.system(size: 11, design: .monospaced)).foregroundStyle(Theme.mutedForeground)
                    .textSelection(.enabled)
            }
            .padding(.top, 4)
        }
        .font(.system(size: 12)).padding(.horizontal, 10)
    }

    /// Which plugin opens each file type is chosen next to the editor
    /// choice under Appearance ▸ Open resources (one place for all openers).
    private var openersRow: some View {
        HStack(spacing: 8) {
            Image(systemName: "arrow.up.forward.app").foregroundStyle(Theme.mutedForeground)
            Text(L10n.t("Choose which plugin opens each file type under Appearance ▸ Open resources."))
                .foregroundStyle(Theme.mutedForeground)
            Spacer(minLength: 8)
            Button(L10n.t("Open resources…")) { store.settingsTab = .appearance }
                .buttonStyle(.bordered).controlSize(.small)
        }
        .font(.system(size: 12)).padding(10)
    }

    @ViewBuilder
    private func installationControl(_ item: PluginSettingsItem) -> some View {
        if isPending(item.id) { ProgressView().controlSize(.small) }
        else if let command = item.installCommand,
                !item.installed || updates[item.id]?.updateAvailable == true {
            Button(item.installed ? L10n.t("Update") : L10n.t("Install")) {
                runInstaller(
                    id: item.id,
                    title: item.installed
                        ? L10n.format("Update %@", item.name)
                        : L10n.format("Install %@", item.name),
                    command: command)
            }
            .buttonStyle(.bordered).controlSize(.small).disabled(!canRunInstaller)
            .help(updateHelp(item))
            .background(PluginDragExclusion(controller: drag))
        } else if !item.installed, let url = item.websiteURL.flatMap(URL.init(string:)) {
            Link(L10n.t("Install"), destination: url).font(.system(size: 11))
                .help(L10n.format("View installation instructions for %@", item.name))
                .background(PluginDragExclusion(controller: drag))
        }
    }

    private func updateHelp(_ item: PluginSettingsItem) -> String {
        if let update = updates[item.id], let current = update.installedVersion {
            if let latest = update.latestVersion {
                return L10n.format("Update %@ to %@", current, latest)
            }
            return L10n.format("Installed: %@", current)
        }
        return item.installedVersion.map { L10n.format("Installed: %@", $0) }
            ?? L10n.t("Run the installer on this Host")
    }

    private func commandEditor(item: PluginSettingsItem, command: RemotePresetSummary, preview: Bool) -> some View {
        PluginCommandEditor(command: command.command, isDraft: false, editable: canEdit && !preview, pending: isPending(item.id),
                            onSave: { value in save(RemotePresetPatch(presetID: command.id, command: value), itemID: item.id) },
                            onCancel: {}) { isHovering in
            Button { addCommand(to: item, after: command) } label: {
                Image(systemName: "plus").font(.system(size: 10, weight: .medium)).frame(width: 18, height: 18)
            }.help(L10n.t("Add another command below")).disabled(!canEdit)
                .opacity(isHovering ? 1 : 0)
                .allowsHitTesting(isHovering)
                .animation(SidebarMotion.reduceMotion ? nil : .easeOut(duration: 0.12), value: isHovering)
        }
        .background(PluginDragExclusion(controller: drag))
        .contextMenu {
            if command.id != item.commands.first?.id {
                Button(L10n.t("Make Default")) { makeDefault(command, item: item) }
            }
            if !command.id.hasPrefix("__agent_default__:") && !command.id.hasPrefix("__app__:") {
                Button(L10n.t("Remove Command"), role: .destructive) {
                    save(RemotePresetPatch(presetID: command.id, removed: true), itemID: item.id)
                }
            }
        }
    }
    private func draftEditor(_ draft: CommandDraft, item: PluginSettingsItem, preview: Bool) -> some View {
        PluginCommandEditor(command: draft.command, isDraft: true, editable: canEdit && !preview, pending: isPending(item.id) || draft.submittedCommand != nil,
            onSave: { value in
                let global = (runtime.snapshot?.presets ?? []).filter { $0.projectID == nil }
                let index = global.firstIndex { $0.id == draft.afterID }.map { $0 + 1 }
                if let draftIndex = drafts.firstIndex(where: { $0.id == draft.id }) {
                    drafts[draftIndex].submittedCommand = value
                    drafts[draftIndex].previousCommandCount = global.filter { $0.command == value }.count
                }
                perform(id: item.id) {
                    do { try await runtime.setPreset(RemotePresetPatch(command: value, quickLaunch: quickValue(item), sortOrder: index)) }
                    catch {
                        if let draftIndex = drafts.firstIndex(where: { $0.id == draft.id }) {
                            drafts[draftIndex].submittedCommand = nil
                        }
                        throw error
                    }
                }
            }, onCancel: { withAnimation(motion) { drafts.removeAll { $0.id == draft.id } } }) { _ in
                Button { withAnimation(motion) { drafts.removeAll { $0.id == draft.id } } } label: {
                    Image(systemName: "xmark").font(.system(size: 10)).frame(width: 18, height: 18)
                }.help(L10n.t("Cancel new command"))
            }
            .background(PluginDragExclusion(controller: drag))
    }
    private func addCommand(to item: PluginSettingsItem, after command: RemotePresetSummary) {
        guard !drafts.contains(where: { $0.pluginID == item.id && $0.afterID == command.id }) else { return }
        withAnimation(motion) { drafts.append(CommandDraft(pluginID: item.id, afterID: command.id, command: command.command)) }
    }
    private func quickValue(_ item: PluginSettingsItem) -> Bool {
        quickOverrides[item.id] ?? item.commands.contains(where: \.quickLaunch)
    }
    private func quickButton(_ item: PluginSettingsItem) -> some View {
        Button { toggleQuick(item) } label: {
            ChromeIconView(icon: .quickPreset, size: 15)
                .foregroundStyle(quickValue(item) ? Color.accentColor : Theme.foreground.opacity(0.2))
                .frame(width: 22, height: 22)
        }
        .buttonStyle(.plain).disabled(!canEdit || isPending(item.id))
        .accessibilityLabel(
            quickValue(item)
                ? L10n.format("Remove %@ from Quick Launch", item.name)
                : L10n.format("Add %@ to Quick Launch", item.name))
        .help(quickValue(item) ? L10n.t("Remove from Quick Launch") : L10n.t("Add to Quick Launch"))
        .background(PluginDragExclusion(controller: drag))
    }
    private func toggleQuick(_ item: PluginSettingsItem) {
        let value = !quickValue(item)
        quickOverrides[item.id] = value
        perform(id: item.id) {
            do {
                for command in item.commands where command.quickLaunch != value {
                    try await runtime.setPreset(RemotePresetPatch(presetID: command.id, quickLaunch: value))
                }
            } catch { quickOverrides.removeValue(forKey: item.id); throw error }
        }
    }
    private func makeDefault(_ command: RemotePresetSummary, item: PluginSettingsItem) {
        let global = (runtime.snapshot?.presets ?? []).filter { $0.projectID == nil }
        guard let firstID = item.commands.first?.id, let index = global.firstIndex(where: { $0.id == firstID }) else { return }
        save(RemotePresetPatch(presetID: command.id, sortOrder: index), itemID: item.id)
    }
    private func save(_ patch: RemotePresetPatch, itemID: String) {
        perform(id: itemID) { try await runtime.setPreset(patch) }
    }
    private var customCommandRow: some View {
        HStack(spacing: 8) {
            Image(systemName: "terminal").foregroundStyle(Theme.mutedForeground)
            TextField(L10n.t("Add custom command…"), text: $customCommand).textFieldStyle(.plain).onSubmit(addCustomCommand)
            Button(L10n.t("Add"), action: addCustomCommand).buttonStyle(.bordered).controlSize(.small)
                .disabled(customCommand.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty || isPending("custom"))
        }.font(.system(size: 12)).padding(10)
    }
    private func addCustomCommand() {
        let command = customCommand.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !command.isEmpty else { return }
        perform(id: "custom") {
            try await runtime.setPreset(RemotePresetPatch(command: command))
            if customCommand == command { customCommand = "" }
        }
    }
    private func moveItem(_ id: String, by step: Int) {
        var order = activeIDs
        guard canReorder, let index = order.firstIndex(of: id), order.indices.contains(index + step) else { return }
        order.swapAt(index, index + step)
        withAnimation(motion) { commitOrder(order) }
    }
    private func commitOrder(_ order: [String]) {
        orderOverride = PluginSettingsList.merging(order, into: items.map(\.id))
        perform(id: "order") {
            do { try await runtime.setPluginOrder(order) }
            catch { withAnimation(motion) { orderOverride = nil }; throw error }
        }
    }
    private func perform(id: String, action: @escaping @MainActor () async throws -> Void) {
        pending[id, default: 0] += 1
        errorMessage = nil
        Task { @MainActor in
            defer { pending[id, default: 1] -= 1 }
            do { try await action() }
            catch { errorMessage = error.localizedDescription }
        }
    }
    private func runInstaller(id: String, title: String, command: String) {
        guard let projectID = runtime.snapshot?.projects.first?.id else { return }
        startingInstallation = title
        perform(id: id) {
            defer { startingInstallation = nil }
            let sessionID = try await runtime.createSession(
                projectID: projectID, command: command, selectOnCreate: false
            )
            installations.append(Installation(id: sessionID, title: title, command: command))
            selectedInstallationID = sessionID
        }
    }

    private func updateTerminalPresentation() {
        runtime.setAuxiliaryTerminalSessions(
            selectedInstallationID.map { Set([$0]) } ?? [], owner: terminalOwner
        )
    }

    private var installationTerminal: some View {
        VStack(alignment: .leading, spacing: 0) {
            Rectangle().fill(Theme.resizerLine).frame(height: 1)
            HStack(spacing: 10) {
                Image(systemName: "terminal")
                    .foregroundStyle(Theme.mutedForeground)
                if let startingInstallation {
                    Text(startingInstallation).fontWeight(.medium)
                    ProgressView().controlSize(.small)
                } else if installations.count > 1 {
                    Picker(L10n.t("Installation terminal"), selection: $selectedInstallationID) {
                        ForEach(installations) { item in
                            Text(item.title).tag(Optional(item.id))
                        }
                    }
                    .labelsHidden()
                    .frame(maxWidth: 260)
                } else if let selectedInstallation {
                    Text(selectedInstallation.title).fontWeight(.medium)
                }
                Spacer()
                Button {
                    runtime.requestImmediateRefresh()
                } label: { Image(systemName: "arrow.clockwise") }
                .help(L10n.format("Refresh installed %@", title.lowercased()))
                Button {
                    selectedInstallationID = nil
                } label: { Image(systemName: "xmark") }
                .disabled(startingInstallation != nil)
                .help(L10n.t("Hide terminal — the session stays in your workspace"))
                .accessibilityLabel(L10n.t("Hide installation terminal"))
            }
            .buttonStyle(.plain)
            .font(.system(size: 12))
            .padding(.horizontal, 16)
            .frame(height: 38)

            if let installation = selectedInstallation, startingInstallation == nil {
                Text(installation.command)
                    .font(.system(size: 11, design: .monospaced))
                    .foregroundStyle(Theme.mutedForeground)
                    .textSelection(.enabled)
                    .lineLimit(1)
                    .padding(.horizontal, 16)
                    .padding(.bottom, 8)
                if let pane = runtime.terminalPane(for: installation.id) {
                    RemoteTerminalPaneHostView(
                        pane: pane,
                        backgroundColor: Theme.terminalBackgroundNSColor,
                        isActive: true
                    )
                    .id(installation.id)
                    .frame(height: 230)
                } else {
                    RemoteTerminalPreparingView(
                        sessionTitle: installation.title, state: runtime.connectionState
                    )
                    .frame(height: 230)
                }
            } else {
                ProgressView(L10n.t("Starting terminal on this Host…"))
                    .frame(maxWidth: .infinity)
                    .frame(height: 230)
            }
        }
        .background(Theme.terminalBackground)
    }

}

private struct PluginCommandEditor<Actions: View>: View {
    let command: String
    let isDraft: Bool
    let editable: Bool
    let pending: Bool
    let onSave: (String) -> Void
    let onCancel: () -> Void
    @ViewBuilder let actions: (Bool) -> Actions
    @ObservedObject private var localization = LocalizationStore.shared
    @State private var text = ""
    @State private var submitted: String?
    @State private var hovering = false
    @FocusState private var focused: Bool

    var body: some View {
        GeometryReader { geometry in
            HStack(spacing: 4) {
                TextField(L10n.t("Launch command"), text: $text)
                    .textFieldStyle(.plain).font(.system(size: 13))
                    .foregroundStyle(Theme.mutedForeground)
                    .focused($focused).onSubmit(commit).onExitCommand(perform: onCancel)
                    .disabled(!editable)
                    .frame(width: min(max(72, (text as NSString).size(withAttributes: [.font: NSFont.systemFont(ofSize: 13)]).width + 12), max(0, geometry.size.width - 22)))
                actions(hovering).buttonStyle(.plain).foregroundStyle(Theme.mutedForeground)
                Spacer(minLength: 0)
            }
        }
        .frame(height: 22)
        .contentShape(Rectangle())
        .onHover { hovering = $0 }
        .onAppear { text = command; if isDraft { focused = true } }
        .onChange(of: command) { value in if !focused { text = value }; submitted = nil }
        .onChange(of: focused) { value in if !value && !isDraft { commit() } }
        .onChange(of: pending) { value in if !value { submitted = nil } }
    }
    private func commit() {
        let value = text.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !value.isEmpty else { if !isDraft { text = command }; return }
        guard !pending, value != submitted, isDraft || value != command else { return }
        submitted = value
        onSave(value)
    }
}
