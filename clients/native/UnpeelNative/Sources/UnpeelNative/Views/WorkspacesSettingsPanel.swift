//
//  WorkspacesSettingsPanel.swift
//  UnpeelNative
//
//  Settings ▸ Workspaces: ONE list of every workspace this app can be in
//  (workspaces-unification) — the default workspace, this instance, registry
//  workspaces on this Mac, and the remote machines this app controls (paired
//  Hosts + SSH Hosts, re-homed from Settings ▸ Remote 2026-08-17; inbound
//  paired phones stay there). Remote rows carry a "Remote" tag instead of
//  living in a second list, rows drag-reorder into a user order shared with
//  the sidebar picker (WorkspaceListOrder), and the two add buttons at the
//  bottom open the shared AddWorkspaceSheet on the matching tab. Selecting a
//  local row scopes the whole app to it over the phase-2 loopback gateway in
//  release and dev builds alike. Remote rows remain development-only.
//

import SwiftUI
import UnpeelShared

struct WorkspacesSettingsPanel: View {
    @ObservedObject var store: UnpeelStore
    /// Opens Settings ▸ Remote (home of the Unpeel Link section since the
    /// standalone Link tab merged into it, 2026-08-13) from the unlicensed
    /// upsell.
    var onOpenPro: (() -> Void)?
    @ObservedObject private var license = LicenseManager.shared
    @State private var rows: [WorkspaceListRowModel] = []
    @State private var draggedRowID: String?
    @State private var renameTarget: LocalRenameTarget?
    @State private var renameText = ""
    @State private var removalCandidate: UnpeelWorkspaceRecord?
    @State private var addSheetMethod: AddWorkspaceSheet.Method?
    @State private var pairControllerPresented = false
    @State private var pairControllerHostName = "Host"
    @State private var errorMessage: String?
    /// Row id whose color-picker popover is open (one at a time).
    @State private var tintPopoverRowID: String?
    /// Read-only, per-home projection of the same devices.json pairing state
    /// shown on Remote Control. This stays UI-only: mutations continue through
    /// the owning workspace's Host/settings paths.
    @State private var sharingByHome: [String: WorkspaceSharingSummary] = [:]

    private let refreshTimer = Timer.publish(every: 3, on: .main, in: .common).autoconnect()

    private struct WorkspaceSharingDevice: Decodable {
        let relayAllowed: Bool?
    }

    private struct WorkspaceSharingFile: Decodable {
        let devices: [WorkspaceSharingDevice]
    }

    private struct WorkspaceSharingSummary: Equatable {
        let deviceCount: Int
        let linkEnabled: Bool

        var label: String {
            guard deviceCount > 0 else { return "Not shared" }
            let devices = deviceCount == 1 ? "1 device" : "\(deviceCount) devices"
            return "\(devices) · Link \(linkEnabled ? "on" : "off")"
        }
    }

    private enum LocalRenameTarget: Identifiable, Equatable {
        case defaultWorkspace
        case record(UnpeelWorkspaceRecord)
        /// A paired or SSH Host — a Controller-local alias (renameHost handles
        /// both). Renaming never touches Host identity or the SSH target.
        case remote(id: String, name: String)

        var id: String {
            switch self {
            case .defaultWorkspace: "default"
            case .record(let record): record.id
            case .remote(let id, _): id
            }
        }

        var currentName: String {
            switch self {
            case .defaultWorkspace:
                UnpeelWorkspaceContext.defaultWorkspaceName ?? "Personal"
            case .record(let record):
                record.name
            case .remote(_, let name):
                name
            }
        }
    }

    var body: some View {
        VStack(alignment: .leading, spacing: 0) {
            Form {
                Section {} header: {
                    SettingsPaneHeader(
                        title: "Workspaces",
                        description: RemoteHostFeature.pickerEnabled
                            ? "Every workspace this app can be in — on this Mac and "
                                + "on remote machines. Selecting one scopes the whole "
                                + "app to it; drag to reorder."
                            : "Every workspace on this Mac has its own sessions, projects, "
                                + "presets, and settings. Selecting one scopes this window to it; "
                                + "drag to reorder."
                    )
                    .padding(.bottom, 4)
                }

                workspacesSection
                selectedWorkspaceColorSection
                if !license.isPro {
                    proRequiredSection
                }
            }
            .formStyle(.grouped)
            .scrollContentBackground(.hidden)
        }
        .onAppear(perform: refresh)
        .onReceive(refreshTimer) { _ in
            // Never reshuffle rows mid-drag; the timer only tracks liveness.
            if draggedRowID == nil { refresh() }
        }
        .onReceive(
            NotificationCenter.default.publisher(for: .unpeelWorkspaceListChanged)
        ) { _ in
            if draggedRowID == nil { refresh() }
        }
        .sheet(item: $addSheetMethod) { method in
            AddWorkspaceSheet(
                store: store,
                hosts: store.remoteHostStore,
                initialMethod: method
            )
        }
        .sheet(isPresented: $pairControllerPresented) {
            RemoteHostPairingSheet(store: store, hostName: pairControllerHostName)
        }
        .alert(
            "Rename workspace",
            isPresented: Binding(
                get: { renameTarget != nil },
                set: { if !$0 { renameTarget = nil } }
            ),
            presenting: renameTarget
        ) { target in
            TextField("Name", text: $renameText)
            Button("Rename") {
                switch target {
                case .defaultWorkspace:
                    UnpeelWorkspaceContext.renameDefaultWorkspace(to: renameText)
                case .record(let record):
                    UnpeelWorkspaceRegistry.rename(id: record.id, to: renameText)
                case .remote(let id, _):
                    _ = store.remoteHostStore.renameHost(id, to: renameText)
                }
                refresh()
                NotificationCenter.default.post(name: .unpeelWorkspaceListChanged, object: nil)
            }
            Button("Cancel", role: .cancel) {}
        } message: { _ in
            Text("Shows in the menu bar and on paired phones. The phone name fully updates after the workspace restarts.")
        }
        .confirmationDialog(
            "Remove \(removalCandidate?.name ?? "workspace")?",
            isPresented: Binding(
                get: { removalCandidate != nil },
                set: { if !$0 { removalCandidate = nil } }
            ),
            titleVisibility: .visible,
            presenting: removalCandidate
        ) { workspace in
            Button("Remove from list", role: .destructive) {
                UnpeelWorkspaceRegistry.remove(id: workspace.id, deleteData: false)
                refresh()
                NotificationCenter.default.post(name: .unpeelWorkspaceListChanged, object: nil)
            }
            Button("Remove and delete its data", role: .destructive) {
                UnpeelWorkspaceRegistry.remove(id: workspace.id, deleteData: true)
                refresh()
                NotificationCenter.default.post(name: .unpeelWorkspaceListChanged, object: nil)
            }
            Button("Cancel", role: .cancel) {}
        } message: { workspace in
            Text(
                "\"Remove from list\" keeps \(workspace.name)'s data at \(workspace.home) so you can "
                    + "re-add it later. Deleting its data also unpairs any phones paired with it."
            )
        }
    }

    // MARK: - The one workspace list

    private var workspacesSection: some View {
        Section {
            ForEach(rows) { row in
                workspaceRow(row)
                    .onDrag {
                        draggedRowID = row.id
                        return NSItemProvider(object: row.id as NSString)
                    }
                    .onDrop(of: [.plainText], delegate: WorkspaceReorderDropDelegate(
                        rowID: row.id,
                        rows: $rows,
                        draggedRowID: $draggedRowID,
                        commit: persistOrder
                    ))
            }

            if license.isPro || RemoteHostFeature.pickerEnabled {
                HStack(spacing: 12) {
                    // One entry point — the sheet's tabs cover this Mac,
                    // pairing, and SSH, so there is no separate "Add Remote".
                    Button {
                        addSheetMethod = .thisMac
                    } label: {
                        Label(
                            RemoteHostFeature.pickerEnabled
                                ? "Add Workspace…" : "New Workspace…",
                            systemImage: "plus"
                        )
                    }
                    Spacer(minLength: 0)
                }
            }

            if let errorMessage {
                Text(errorMessage)
                    .font(.system(size: 11))
                    .foregroundStyle(.red.opacity(0.9))
            }
        } header: {
            SettingsSectionHeader(
                title: "All workspaces",
                description: RemoteHostFeature.pickerEnabled
                    ? "Click the color dot to set a workspace's color, or the ⋯ menu "
                        + "to rename or remove it. Drag to reorder — the sidebar "
                        + "switcher matches. Add Workspace… covers this Mac, pairing, and SSH."
                    : "Click a workspace to select it. Set each one's color with its dot, "
                        + "use the ⋯ menu to rename or remove it, and drag to reorder — "
                        + "the sidebar switcher matches."
            )
        }
    }

    @ViewBuilder
    private func workspaceRow(_ row: WorkspaceListRowModel) -> some View {
        HStack(spacing: 8) {
            tintDot(row)

            // Local workspace switching ships independently of the dev-only
            // remote Host picker, so release rows remain usable after a
            // relaunch. Dev builds add paired/SSH rows to the same path.
            if WorkspaceFeature.pickerEnabled {
                Button {
                    select(row)
                } label: {
                    rowLabel(row)
                        .contentShape(Rectangle())
                }
                .buttonStyle(.plain)
            } else {
                rowLabel(row)
            }

            if isScoped(row) {
                Image(systemName: "checkmark.circle.fill")
                    .foregroundStyle(Theme.accent)
            }

            if sharingSummary(for: row) != nil {
                Button("Share…") {
                    store.openSettings(tab: .mobile)
                }
                .buttonStyle(.bordered)
                .controlSize(.small)
                .help("Open \(row.name)'s Remote Control settings")
            }

            rowMenu(row)
        }
        .padding(.vertical, 2)
        .contextMenu { rowMenuItems(row) }
    }

    /// Leading color dot: shows this workspace's App color and opens the
    /// swatch picker. For the current instance it applies live; for another
    /// workspace it writes that workspace's own color (see setRowTint).
    private func tintDot(_ row: WorkspaceListRowModel) -> some View {
        Button {
            tintPopoverRowID = row.id
        } label: {
            Circle()
                .fill(row.tint.swatch)
                .frame(width: 12, height: 12)
                .overlay(
                    Circle().strokeBorder(Color.white.opacity(0.5), lineWidth: 1)
                )
                .frame(width: 20, height: 20)
                .contentShape(Rectangle())
        }
        .buttonStyle(.plain)
        .help("Set \(row.name)'s color")
        .popover(
            isPresented: Binding(
                get: { tintPopoverRowID == row.id },
                set: { if !$0 { tintPopoverRowID = nil } }
            ),
            arrowEdge: .bottom
        ) {
            HStack(spacing: 12) {
                ForEach(AppTint.allCases) { tint in
                    AppTintSwatch(tint: tint, isSelected: row.tint == tint) {
                        setRowTint(tint, row: row)
                        tintPopoverRowID = nil
                    }
                }
            }
            .padding(14)
        }
    }

    /// Trailing ⋯ menu: rename / remove / forget, per workspace kind.
    private func rowMenu(_ row: WorkspaceListRowModel) -> some View {
        Menu {
            rowMenuItems(row)
        } label: {
            Image(systemName: "ellipsis")
                .font(.system(size: 13, weight: .semibold))
                .foregroundStyle(Theme.mutedForeground)
                .frame(width: 22, height: 22)
                .contentShape(Rectangle())
        }
        .menuStyle(.borderlessButton)
        .menuIndicator(.hidden)
        .fixedSize()
        .help("More…")
    }

    @ViewBuilder
    private func rowMenuItems(_ row: WorkspaceListRowModel) -> some View {
        switch row.kind {
        case let .local(record, isDefault, _, isRunning):
            if isDefault || record != nil {
                Button("Rename…") {
                    beginRename(record.map { .record($0) } ?? .defaultWorkspace)
                }
            }
            if let record {
                Divider()
                Button("Remove…", role: .destructive) {
                    if isRunning {
                        errorMessage = "Quit \(record.name) before removing it."
                    } else {
                        removalCandidate = record
                    }
                }
            }
        case .paired(let host):
            if canAddPhone(to: host) {
                Button("Add iPhone or iPad…") {
                    pairControllerHostName = host.name
                    pairControllerPresented = true
                }
            }
            Button("Rename…") {
                beginRename(.remote(id: host.hostID, name: host.name))
            }
            Divider()
            Button("Forget Workspace", role: .destructive) {
                store.forgetHost(host.hostID)
                refresh()
            }
        case .ssh(let host):
            Button("Rename…") {
                beginRename(.remote(id: host.id, name: host.name))
            }
            Divider()
            Button("Forget Workspace", role: .destructive) {
                store.forgetHost(host.id)
                refresh()
            }
        }
    }

    private func beginRename(_ target: LocalRenameTarget) {
        renameText = target.currentName
        renameTarget = target
    }

    private func rowLabel(_ row: WorkspaceListRowModel) -> some View {
        HStack(spacing: 10) {
            Image(systemName: row.icon)
                .frame(width: 18)
            VStack(alignment: .leading, spacing: 1) {
                HStack(spacing: 6) {
                    Text(row.name)
                        .font(.system(size: 13))
                        .foregroundStyle(Theme.foreground)
                    if let badge = row.badge {
                        Text(badge)
                            .font(.system(size: 10, weight: .medium))
                            .foregroundStyle(Theme.mutedForeground)
                            .padding(.horizontal, 6)
                            .padding(.vertical, 2)
                            .background(Theme.mutedForeground.opacity(0.14), in: Capsule())
                    }
                }
                Text(row.detail)
                    .font(.system(size: 10, design: .monospaced))
                    .foregroundStyle(Theme.mutedForeground)
                    .lineLimit(1)
                    .truncationMode(.middle)
                if let sharing = sharingSummary(for: row) {
                    Text(sharing.label)
                        .font(.system(size: 10, weight: .medium))
                        .foregroundStyle(Theme.mutedForeground)
                        .lineLimit(1)
                }
            }
            Spacer(minLength: 12)
        }
    }

    /// Apply a picked App color to the workspace on `row`. Current instance:
    /// live via the store. Another local workspace: write its own defaults
    /// suite (raw home so the hash matches the launcher) and, if it is
    /// running, ping `/reload-appearance` so it recolors now. Remote: a
    /// Controller-local label color (no host push yet).
    private func setRowTint(_ tint: AppTint, row: WorkspaceListRowModel) {
        switch row.kind {
        case let .local(record, isDefault, isCurrentInstance, _):
            if isCurrentInstance {
                store.setAppTint(tint)
            } else if let record {
                AppDefaults.suite(forUnpeelHome: record.home)
                    .set(tint.rawValue, forKey: UnpeelStore.nativeAppTintKey)
                Self.pingReloadAppearance(
                    homeDir: URL(fileURLWithPath: record.home, isDirectory: true)
                )
            } else if isDefault {
                UserDefaults.standard
                    .set(tint.rawValue, forKey: UnpeelStore.nativeAppTintKey)
                Self.pingReloadAppearance(
                    homeDir: UnpeelWorkspaceRegistry.realUnpeelDir
                )
            }
        case .paired(let host):
            RemoteWorkspaceTint.set(tint, hostID: host.hostID)
        case .ssh(let host):
            RemoteWorkspaceTint.set(tint, hostID: host.id)
        }
        // If we're currently viewing the workspace we just recolored, repaint
        // the chrome to match (the current-instance case already did via
        // setAppTint's live animation).
        if !isCurrentInstance(row) {
            store.applyScopeTint()
        }
        // The sidebar workspace selector caches its dots — tell it to re-read.
        NotificationCenter.default.post(name: .unpeelWorkspaceTintChanged, object: nil)
        refresh()
    }

    private func isCurrentInstance(_ row: WorkspaceListRowModel) -> Bool {
        if case let .local(_, _, isCurrentInstance, _) = row.kind {
            return isCurrentInstance
        }
        return false
    }

    /// Best-effort POST /reload-appearance to every port in the target home's
    /// `app-ports` (mirrors the selector's /show-window ping). Internal: the
    /// scoped Appearance editor uses the same cross-suite write + ping.
    static func pingReloadAppearance(homeDir: URL) {
        let portsFile = homeDir.appendingPathComponent("app-ports")
        // The canonical worker consumes this peer ping to refresh its cached
        // overlay; `/reload-appearance` remains the native-window repaint.
        UnpeelStore.announceStateChange(
            "native-overlay",
            registry: portsFile,
            ownPort: nil
        )
        guard let contents = try? String(contentsOf: portsFile, encoding: .utf8)
        else { return }
        for line in contents.split(whereSeparator: \.isNewline) {
            guard let port = Int(line.trimmingCharacters(in: .whitespaces)),
                  (1...65535).contains(port),
                  let url = URL(string: "http://127.0.0.1:\(port)/reload-appearance")
            else { continue }
            var request = URLRequest(url: url, timeoutInterval: 2)
            request.httpMethod = "POST"
            URLSession.shared.dataTask(with: request).resume()
        }
    }

    /// Shared switch semantics with the sidebar picker, footer dots, and
    /// sidebar swipe — one implementation in WorkspaceSwitching.
    private func isScoped(_ row: WorkspaceListRowModel) -> Bool {
        WorkspaceSwitching.isScoped(row, store: store)
    }

    private func select(_ row: WorkspaceListRowModel) {
        errorMessage = nil
        // Single-slide pager switch where the sidebar list is visible;
        // settings-open rescopes fall back to the instant switch inside.
        WorkspaceSwitching.selectSliding(row, store: store)
    }

    private func canAddPhone(to host: PairedHostRecord) -> Bool {
        guard store.selectedHostScope.remoteHostID == host.hostID,
              store.remoteHostRuntime.snapshot?.macID == host.hostID
        else {
            return false
        }
        return store.remoteHostRuntime.supportsHostOperation(
            RemoteHostRuntime.HostOperation.pairingInvitation
        )
    }

    // MARK: - Other sections

    /// Shown under the list when Pro isn't active. Workspaces are a Link
    /// feature; already-running workspace instances are not touched — this
    /// only gates the management UI.
    private var proRequiredSection: some View {
        Section {
            HStack(alignment: .firstTextBaseline, spacing: 10) {
                Image(systemName: "lock.fill")
                    .font(.system(size: 12, weight: .medium))
                    .foregroundStyle(Theme.mutedForeground)
                VStack(alignment: .leading, spacing: 4) {
                    Text("Extra workspaces require Unpeel Link")
                        .font(.system(size: 13, weight: .semibold))
                        .foregroundStyle(Theme.foreground)
                    Text("Unpeel Link adds workspaces, remote control from your iPhone, and "
                        + "Unpeel Remote access from anywhere — $59 per seat per year.")
                        .font(.system(size: 12))
                        .foregroundStyle(Theme.mutedForeground)
                        .fixedSize(horizontal: false, vertical: true)
                    if let onOpenPro {
                        Button("Open Unpeel Link Settings", action: onOpenPro)
                            .controlSize(.small)
                            .padding(.top, 4)
                    }
                }
            }
            .padding(.vertical, 4)
        }
    }

    /// App color for the workspace currently scoped — the same swatches the
    /// per-row dot popover offers (which stays), surfaced here because a dot
    /// popover alone proved too hidden.
    @ViewBuilder
    private var selectedWorkspaceColorSection: some View {
        if let row = rows.first(where: isScoped) {
            Section {
                HStack(spacing: 12) {
                    ForEach(AppTint.allCases) { tint in
                        AppTintSwatch(tint: tint, isSelected: row.tint == tint) {
                            setRowTint(tint, row: row)
                        }
                    }
                    Spacer(minLength: 0)
                }
                .padding(.vertical, 2)
            } header: {
                SettingsSectionHeader(
                    title: "App color for \(row.name)",
                    description: "Washes \(row.name)'s window chrome and shows on "
                        + "its paired phones. Any other workspace's color is set "
                        + "from its row's dot in the list above."
                )
            }
        }
    }

    // MARK: - Data

    private func refresh() {
        let refreshedRows = WorkspaceListOrder.apply(
            to: Self.buildRows(
                store: store,
                includeExtraLocal: license.isPro
            ),
            key: \.id
        )
        rows = refreshedRows
        guard UnpeelFeatureFlags.mobileRemoteControlEnabled else {
            sharingByHome = [:]
            return
        }
        sharingByHome = Dictionary(
            uniqueKeysWithValues: refreshedRows.compactMap {
                row -> (String, WorkspaceSharingSummary)? in
                guard case .local = row.kind, let home = row.home else { return nil }
                return (home, Self.loadSharingSummary(home: home))
            }
        )
    }

    /// Only the selected local workspace gets sharing chrome. Paired/SSH rows
    /// retain their existing remote detail and actions, and release builds do
    /// not change while Remote Control remains behind its existing dev gate.
    private func sharingSummary(for row: WorkspaceListRowModel) -> WorkspaceSharingSummary? {
        guard UnpeelFeatureFlags.mobileRemoteControlEnabled,
              isScoped(row),
              case .local = row.kind,
              let home = row.home
        else { return nil }
        return sharingByHome[home] ?? WorkspaceSharingSummary(
            deviceCount: 0,
            linkEnabled: false
        )
    }

    private static func loadSharingSummary(home: String) -> WorkspaceSharingSummary {
        let url = URL(fileURLWithPath: home, isDirectory: true)
            .appendingPathComponent("mobile")
            .appendingPathComponent("devices.json")
        guard let data = try? Data(contentsOf: url),
              let file = try? JSONDecoder().decode(WorkspaceSharingFile.self, from: data)
        else {
            return WorkspaceSharingSummary(deviceCount: 0, linkEnabled: false)
        }
        return WorkspaceSharingSummary(
            deviceCount: file.devices.count,
            linkEnabled: file.devices.contains { $0.relayAllowed != false }
        )
    }

    private func persistOrder() {
        WorkspaceListOrder.save(rows.map(\.id))
    }

    /// Shared row builder: the sidebar picker renders the same unified,
    /// user-ordered model.
    static func buildRows(
        store: UnpeelStore,
        includeExtraLocal: Bool
    ) -> [WorkspaceListRowModel] {
        let registry = UnpeelWorkspaceRegistry.load()
        let defaultHome = UnpeelWorkspaceRegistry.realUnpeelDir
        let normalizedDefault = UnpeelWorkspaceRegistry.normalizePath(defaultHome.path)
        let currentHome = UnpeelWorkspaceContext.isDefaultInstance
            ? normalizedDefault
            : UnpeelWorkspaceRegistry.normalizePath(LaunchConfig.unpeelDir.path)

        var rows: [WorkspaceListRowModel] = []
        let defaultIsCurrent = currentHome == normalizedDefault
        let defaultRunning = defaultIsCurrent
            || UnpeelWorkspaceLauncher.runningPid(home: defaultHome) != nil
        rows.append(WorkspaceListRowModel(
            id: WorkspaceListOrder.localKey(home: defaultHome.path),
            name: UnpeelWorkspaceContext.defaultWorkspaceName ?? "Personal",
            detail: defaultHome.path,
            icon: "desktopcomputer",
            badge: defaultIsCurrent ? "This instance" : defaultRunning ? "Running" : nil,
            home: normalizedDefault,
            // The default instance's tint lives in `.standard` (no UNPEEL_HOME).
            tint: defaultIsCurrent
                ? store.appTint
                : localStoredTint(home: nil),
            kind: .local(
                record: nil,
                isDefault: true,
                isCurrentInstance: defaultIsCurrent,
                isRunning: defaultRunning
            )
        ))
        for record in registry {
            let normalized = UnpeelWorkspaceRegistry.normalizePath(record.home)
            let isCurrent = normalized == currentHome
            // Without Pro, extra workspaces are managed elsewhere; the list
            // still shows the one this instance IS so the screen never
            // contradicts reality.
            guard includeExtraLocal || isCurrent else { continue }
            let isRunning = isCurrent
                || UnpeelWorkspaceLauncher.runningPid(
                    home: URL(fileURLWithPath: record.home, isDirectory: true)
                ) != nil
            rows.append(WorkspaceListRowModel(
                id: WorkspaceListOrder.localKey(home: record.home),
                name: record.name,
                detail: record.home,
                icon: "desktopcomputer",
                badge: isCurrent ? "This instance" : isRunning ? "Running" : nil,
                home: normalized,
                tint: isCurrent ? store.appTint : localStoredTint(home: record.home),
                kind: .local(
                    record: record,
                    isDefault: false,
                    isCurrentInstance: isCurrent,
                    isRunning: isRunning
                )
            ))
        }
        // A dev-blank instance runs from an unregistered UNPEEL_HOME; give it
        // a row (named after its folder) so the current instance always
        // appears in its own list.
        if !rows.contains(where: { $0.home == currentHome }) {
            rows.append(WorkspaceListRowModel(
                id: WorkspaceListOrder.localKey(home: currentHome),
                name: URL(fileURLWithPath: currentHome).lastPathComponent,
                detail: currentHome,
                icon: "desktopcomputer",
                badge: "This instance",
                home: currentHome,
                tint: store.appTint,
                kind: .local(
                    record: nil,
                    isDefault: false,
                    isCurrentInstance: true,
                    isRunning: true
                )
            ))
        }

        guard RemoteHostFeature.pickerEnabled else { return rows }
        for host in store.remoteHostStore.records {
            // Only the SELECTED remote host has a live bootstrap, so only it can
            // advertise its hardware; non-selected paired hosts fall back to the
            // generic rack icon and the raw hostID.
            let liveSnapshot = store.remoteHostRuntime.snapshot?.macID == host.hostID
                ? store.remoteHostRuntime.snapshot
                : nil
            // A Box Host shows its environment (Lane D); otherwise the
            // advertised hardware model, then the raw id.
            let detail = liveSnapshot?.hostEnvironment?.rowLabel
                ?? liveSnapshot?.hostDeviceModel
                ?? host.hostID
            rows.append(WorkspaceListRowModel(
                id: WorkspaceListOrder.pairedKey(hostID: host.hostID),
                name: host.name,
                detail: detail,
                icon: hostDeviceIcon(kind: liveSnapshot?.hostDeviceKind),
                badge: "Remote",
                home: nil,
                tint: RemoteWorkspaceTint.get(host.hostID),
                kind: .paired(host)
            ))
        }
        for host in store.remoteHostStore.sshRecords {
            rows.append(WorkspaceListRowModel(
                id: WorkspaceListOrder.sshKey(id: host.id),
                name: host.name,
                detail: "SSH · \(host.destination)",
                icon: "terminal",
                badge: "Remote",
                home: nil,
                tint: RemoteWorkspaceTint.get(host.id),
                kind: .ssh(host)
            ))
        }
        return rows
    }

    /// SF Symbol for a remote Host's advertised hardware kind
    /// (`RemoteBootstrapSnapshot.hostDeviceKind`). Nil/unknown — including a
    /// paired host with no live bootstrap yet — falls back to the rack icon.
    private static func hostDeviceIcon(kind: String?) -> String {
        switch kind {
        case "macbook": return "laptopcomputer"
        case "macMini": return "macmini"
        case "macStudio": return "macstudio"
        case "imac": return "desktopcomputer"
        case "macPro": return "macpro.gen3"
        case "linux": return "server.rack"
        default: return "server.rack"
        }
    }

    /// A local workspace's stored App color read from its own defaults suite
    /// (nil home = the default instance's `.standard`). Pass the raw home the
    /// launcher uses as UNPEEL_HOME so the suite hash matches.
    private static func localStoredTint(home: String?) -> AppTint {
        AppDefaults.suite(forUnpeelHome: home)
            .string(forKey: UnpeelStore.nativeAppTintKey)
            .flatMap(AppTint.init(rawValue:)) ?? .none
    }

    // MARK: - Actions

}

/// One row of the unified workspace list. The id doubles as the persisted
/// order key (WorkspaceListOrder); `home` is set for local rows only.
struct WorkspaceListRowModel: Identifiable {
    enum Kind {
        case local(
            record: UnpeelWorkspaceRecord?,
            isDefault: Bool,
            isCurrentInstance: Bool,
            isRunning: Bool
        )
        case paired(PairedHostRecord)
        case ssh(SSHHostRecord)
    }

    let id: String
    let name: String
    let detail: String
    let icon: String
    let badge: String?
    let home: String?
    /// This workspace's stored App color, for the per-row color dot. Local
    /// rows read their own defaults suite; remote rows carry a Controller-
    /// local label color (no protocol push to the host yet).
    let tint: AppTint
    let kind: Kind
}

/// Controller-local App color for a remote workspace, keyed by Host id. The
/// host owns its real tint (advertised as bootstrap `hostTintHue`); this is a
/// local label color so your machines are tellable apart in the list and
/// sidebar before/without a live connection. Phase 3 can prefer the live
/// value when connected and fall back to this.
enum RemoteWorkspaceTint {
    private static let key = "unpeel.native.remoteWorkspaceTint"

    static func get(_ hostID: String) -> AppTint {
        (UserDefaults.standard.dictionary(forKey: key)?[hostID] as? String)
            .flatMap(AppTint.init(rawValue:)) ?? .none
    }

    static func set(_ tint: AppTint, hostID: String) {
        var map = UserDefaults.standard.dictionary(forKey: key) as? [String: String] ?? [:]
        map[hostID] = tint.rawValue
        UserDefaults.standard.set(map, forKey: key)
    }
}

/// Live-move reorder for the unified list: rows shift as the drag passes
/// over them (the sidebar's established pattern), and the final order is
/// persisted once on drop.
private struct WorkspaceReorderDropDelegate: DropDelegate {
    let rowID: String
    @Binding var rows: [WorkspaceListRowModel]
    @Binding var draggedRowID: String?
    let commit: () -> Void

    func dropEntered(info _: DropInfo) {
        guard let draggedRowID,
              draggedRowID != rowID,
              let from = rows.firstIndex(where: { $0.id == draggedRowID }),
              let to = rows.firstIndex(where: { $0.id == rowID })
        else { return }
        withAnimation(.easeInOut(duration: 0.12)) {
            rows.move(
                fromOffsets: IndexSet(integer: from),
                toOffset: to > from ? to + 1 : to
            )
        }
    }

    func dropUpdated(info _: DropInfo) -> DropProposal? {
        DropProposal(operation: .move)
    }

    func performDrop(info _: DropInfo) -> Bool {
        commit()
        draggedRowID = nil
        return true
    }

    func dropExited(info _: DropInfo) {}
}
