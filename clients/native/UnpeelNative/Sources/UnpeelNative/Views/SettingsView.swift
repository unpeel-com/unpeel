//
//  SettingsView.swift
//  UnpeelNative
//
//  Settings presentation, deliberately different from the Svelte
//  full-screen swap (designer's spec, 2026-06-12): the app layout never
//  hides. Instead:
//  - SettingsSidebarPanel: the Back row + tab nav. It slides into the
//    sidebar's list area using the exact worktrees-panel motion
//    (offset ±140 + fade, 0.2s cubicOut — SidebarMotion.slide); the
//    footer stays put, same as the worktrees slide.
//  - SettingsContentHost: the right-hand pane — centered
//    "Settings / <Tab>" titlebar (SettingsView.svelte:100-127) over the
//    active panel. ContentArea swaps it with the terminal INSTANTLY
//    (transition .identity, animation nil): the Ghostty surface is
//    Metal-backed, and animating opacity across a CAMetalLayer is what
//    produced the old full-window blink. Tab switching swaps panels
//    in place with no transition, like the Svelte tab switch.
//  Esc returns to the workspace (SettingsView.svelte:45-50).
//
//  Panel inventory vs the Svelte SETTINGS_TABS (settings/tabs.ts:9-35):
//  - Appearance → AppearanceSettingsPanel (mode only — light/dark/system;
//                 no ambience presets natively yet). Persists as a native
//                 UserDefaults overlay over the read-only app-state.json
//                 `theme` (the native app must never write that file).
//  - Presets   → PresetsSettingsPanel (full parity, native overlay storage)
//  - Worktrees → WorktreesSettingsPanel (gated on the Git worktrees
//                experiment: agent-worktree discovery + this workspace's list)
//  - Advanced  → AdvancedSettingsPanel (resource diagnostics, old-session
//                cleanup, and the gear menu's folder/log utilities)
//  - General and Tags are omitted from the nav: their machinery
//    (code-editor preference, tag CRUD) does not exist in the native build
//    and both persist via app-state.json writes.
//

import AppKit
import CoreImage
import SwiftUI
import UnpeelShared

// MARK: - Tabs (settings/tabs.ts)

enum SettingsTab: String, CaseIterable, Identifiable {
    // Workspaces leads the nav: the workspace list is the primary scope
    // surface, and Settings edits per-workspace state (see
    // the scope selector below). Remote keeps inbound
    // devices, Link, and legacy license.
    case workspaces
    case presets
    case appearance
    case mobile
    case transcripts
    case notifications
    case sessions
    case browser
    case computer
    case worktrees
    case experimental
    case advanced
    // The standalone "Unpeel Link" license tab was merged into Remote
    // (2026-08-13): license + seat status now render as a section of the
    // Remote panel, next to the device enrollment list. Legacy deep links
    // ("license") are mapped to .mobile in Snapshot restore.

    var id: String { rawValue }

    /// `profiles` was the released developer deep-link spelling before the
    /// isolated-instance feature became Workspaces. It was never persisted as
    /// app state, but accepting it keeps existing snapshot/dev commands valid.
    static func compatibleRawValue(_ rawValue: String) -> SettingsTab? {
        SettingsTab(rawValue: rawValue == "profiles" ? "workspaces" : rawValue)
    }

    static var visibleCases: [SettingsTab] {
        visibleCases(computerUseControllable: false)
    }

    /// `computerUseControllable`: the selected Host advertises computer use
    /// in its bootstrap (`UnpeelFeatureFlags.computerUseControllable`). The
    /// Computer tab then shows in every build flavor for that Host; the
    /// local Mac scope in a release build keeps today's behavior (hidden).
    static func visibleCases(computerUseControllable: Bool) -> [SettingsTab] {
        allCases.filter { tab in
            switch tab {
            case .mobile: return UnpeelFeatureFlags.mobileRemoteControlEnabled
            // Sessions MCP is experimental (Settings ▸ Experimental); its
            // panel only exists while the feature is on.
            case .sessions: return UnpeelFeatureFlags.isEnabled(.sessionsMcp)
            // Browser and computer use are experimental too; their panels
            // only exist while the features are on.
            case .browser: return UnpeelFeatureFlags.isEnabled(.browserMcp)
            // Keep the policy panel reachable in development even while the
            // experiment is off; remote Hosts need it to move from Off to
            // Ask/Allow before their adapter can become ready.
            case .computer:
                return UnpeelFeatureFlags.isAvailable(.computerUse) || computerUseControllable
            case .workspaces: return UnpeelFeatureFlags.isEnabled(.workspaces)
            // Git worktrees is experimental; its panel only exists while
            // the feature is on (same live gate as the sidebar folders).
            case .worktrees: return UnpeelFeatureFlags.isEnabled(.worktrees)
            default: return true
            }
        }
    }

    /// The tabs that follow the Settings scope dropdown to the selected
    /// workspace/Host — i.e. whose settings operations exist on the Host
    /// contract. Grows as verbs land;
    /// `settings.presets.set` is the first.
    static var hostScopedCases: [SettingsTab] {
        [
            .presets, .appearance, .transcripts, .notifications, .sessions,
            .browser, .computer, .experimental, .advanced,
        ]
    }

    var title: String {
        switch self {
        case .appearance: return "Appearance"
        case .presets: return "Agents & Apps"
        case .mobile: return "Remote Control"
        case .workspaces: return "Workspaces"
        case .transcripts: return "Transcripts"
        case .notifications: return "Notifications"
        case .sessions: return "Sessions use"
        case .browser: return "Browser use"
        case .computer: return "Computer use"
        case .worktrees: return "Worktrees"
        case .experimental: return "Experimental"
        case .advanced: return "Advanced"
        }
    }

    /// Glass-gradient nav icon (ChromeIcons.swift) — same treatment as the
    /// sidebar folder/gear marks.
    var icon: ChromeIcon {
        switch self {
        case .appearance: return .settingsAppearance
        case .presets: return .settingsPresets
        case .mobile: return .settingsRemote
        case .workspaces: return .settingsWorkspaces
        case .transcripts: return .settingsTranscripts
        case .notifications: return .settingsNotifications
        case .sessions: return .settingsSessions
        case .browser: return .settingsBrowser
        case .computer: return .settingsComputer
        case .worktrees: return .settingsWorktrees
        case .experimental: return .settingsExperimental
        case .advanced: return .settingsAdvanced
        }
    }

    /// The first-party MCP domain panels, grouped under an "Unpeel MCP"
    /// header at the bottom of the sidebar nav (one unified server, one
    /// domain per panel).
    var isBuiltInMCP: Bool {
        switch self {
        case .sessions, .browser, .computer: return true
        default: return false
        }
    }
}

// MARK: - Sidebar nav panel (.settings-sidebar nav)

/// The Back row + tab nav that slides into the sidebar's list area while
/// settings is open (SidebarView hosts it in the same ZStack as the
/// project tree, with the same ±140 slide). No background here — it
/// rides on the sidebar's existing chrome; list padding
/// `calc(titlebar + 2px) 8px 12px`, gap 2 (SettingsView.svelte:173-179).
struct SettingsSidebarPanel: View {
    @ObservedObject var store: UnpeelStore

    private static let feedbackURL = URL(string: "https://github.com/orgs/unpeel-com/discussions")!

    var body: some View {
        VStack(spacing: 0) {
            // The nav scrolls when the window is short, dissolving into the
            // top chrome / bottom footer through the same fade mask as the
            // main sidebar lists.
            ScrollView {
                VStack(alignment: .leading, spacing: 2) {
                    SettingsNavRow(
                        title: "Back",
                        leadingIcon: .back,
                        isActive: false,
                        action: { store.closeSettings() }
                    )
                    .padding(.bottom, 6) // .back-row margin-bottom 6

                    // The Settings scope dropdown (the
                    // scope rule): always
                    // present, showing the ACTIVE workspace. Everything below
                    // it except the Workspaces registry row IS that
                    // workspace's settings — appearance included, since each
                    // workspace has its own defaults suite and theme.
                    settingsScopePicker
                        .padding(.bottom, 6)

                    // .settings-nav-list, gap 1.5
                    VStack(alignment: .leading, spacing: 1.5) {
                        // The ONE machine-level tab: the shared workspace
                        // registry (create/open/remove). It sits directly
                        // under the dropdown, outside any workspace's own
                        // settings.
                        if SettingsTab.visibleCases(computerUseControllable: store.selectedHostAdvertisesComputerUse).contains(.workspaces) {
                            SettingsNavRow(
                                title: SettingsTab.workspaces.title,
                                leadingIcon: SettingsTab.workspaces.icon,
                                isActive: selectedTab == .workspaces,
                                action: { store.settingsTab = .workspaces }
                            )
                            .padding(.bottom, 6)
                        }

                        // The ACTIVE workspace's settings. Host-backed tabs
                        // stay visible for SSH/headless Hosts; capability and
                        // additive-payload checks happen in the content pane.
                        ForEach(SettingsTab.visibleCases(computerUseControllable: store.selectedHostAdvertisesComputerUse).filter { tab in
                            if tab.isBuiltInMCP || tab == .workspaces { return false }
                            return true
                        }) { tab in
                            SettingsNavRow(
                                title: tab.title,
                                leadingIcon: tab.icon,
                                isActive: tab == selectedTab,
                                action: { store.settingsTab = tab }
                            )
                        }

                        // Built-in MCP panels (per-workspace access policies)
                        // grouped under their own header.
                        SettingsNavSectionHeader(title: "Unpeel MCP")
                            .padding(.top, 10)

                        ForEach(SettingsTab.visibleCases(computerUseControllable: store.selectedHostAdvertisesComputerUse).filter(\.isBuiltInMCP)) { tab in
                            SettingsNavRow(
                                title: tab.title,
                                leadingIcon: tab.icon,
                                isActive: tab == selectedTab,
                                action: { store.settingsTab = tab }
                            )
                        }

                    }
                }
                // Bottom padding keeps the last row clear of the mask's
                // 26pt bottom fade when scrolled to the end.
                .padding(EdgeInsets(
                    top: Theme.titlebarHeight + 2, leading: 8, bottom: 26, trailing: 8
                ))
                .frame(maxWidth: .infinity, alignment: .topLeading)
            }
            .scrollIndicators(.hidden)
            .mask(SidebarListFadeMask())

            // Pinned at the bottom: opens GitHub Discussions (same as the
            // website footer's "Bugs & Feedback" link and the iOS sidebar).
            // Borderless like the main sidebar footer — the list's bottom
            // fade provides the separation.
            Link(destination: Self.feedbackURL) {
                HStack(spacing: 8) {
                    Image(systemName: "exclamationmark.bubble")
                        .font(.system(size: 12, weight: .medium))
                    Text("Feedback & bugs")
                        .font(.system(size: 12.5, weight: .medium))
                    Spacer(minLength: 4)
                    Image(systemName: "arrow.up.right")
                        .font(.system(size: 10, weight: .semibold))
                        .opacity(0.6)
                }
                .foregroundStyle(.secondary)
                .padding(.horizontal, 10)
                .padding(.vertical, 7)
                .frame(maxWidth: .infinity, alignment: .leading)
                .contentShape(Rectangle())
            }
            .buttonStyle(.plain)
            .padding(EdgeInsets(top: 0, leading: 8, bottom: 12, trailing: 8))
        }
        .frame(maxWidth: .infinity, maxHeight: .infinity, alignment: .topLeading)
        .overlay(alignment: .top) {
            // .settings-sidebar-drag-region: titlebar-height drag strip.
            WindowDragArea().frame(height: Theme.titlebarHeight)
        }
    }

    private var selectedTab: SettingsTab {
        if SettingsTab.visibleCases(computerUseControllable: store.selectedHostAdvertisesComputerUse).contains(store.settingsTab) {
            return store.settingsTab
        }
        // The selected tab's gate (Mobile dev flag, Sessions MCP experiment)
        // turned off — fall back to the first tab. Workspaces leads the enum
        // but is itself gated, so resolve through visibleCases.
        return SettingsTab.visibleCases(computerUseControllable: store.selectedHostAdvertisesComputerUse).first ?? .presets
    }

    /// The scope dropdown: the SAME workspace picker as the sidebar footer
    /// dots (`WorkspacePickerPanel`), anchored at the top of the Settings
    /// nav. The label always names the ACTIVE workspace; picking a row
    /// switches the window's scope and Settings follows it.
    private var settingsScopePicker: some View {
        SettingsScopePickerControl(store: store)
    }

}

/// The Settings sidebar's scope control: a picker-row-styled button showing
/// the active workspace's tint dot and name, opening the shared
/// `WorkspacePickerPanel` (the sidebar dots' popover) directly below it.
/// Selecting a row switches the whole window's active workspace — Settings'
/// workspace-scoped tabs follow. Local workspace switching is available in
/// release builds; only paired/SSH rows remain development-only.
/// Appearance for the ACTIVE local workspace, edited from this window: every
/// control writes the workspace's own defaults suite and pings
/// `/reload-appearance` so a running instance restyles live — the same
/// pattern the Workspaces tab's color pickers established. Mode and color
/// preview in this window immediately because its chrome follows the scope;
/// transparency renders in the workspace's own windows.
private struct HostAppearanceSettingsPanel: View {
    @ObservedObject var store: UnpeelStore
    let home: String
    let name: String

    @State private var mode: ThemePreference = .system
    @State private var tint: AppTint = .none
    @State private var backgroundOpacity = TransparencyModel.backgroundMaterialOpacity
    @State private var surfaceOpacity = 1.0
    @State private var backgroundTone = TransparencyModel.designBackgroundTone
    @State private var surfaceTone = TransparencyModel.designSurfaceTone
    @State private var loaded = false
    /// Slider drags fire per tick — coalesce the target-instance ping.
    @State private var pingWorkItem: DispatchWorkItem?

    private var suite: UserDefaults { AppDefaults.suite(forUnpeelHome: home) }

    var body: some View {
        VStack(alignment: .leading, spacing: 0) {
            Form {
                Section {} header: {
                    SettingsPaneHeader(
                        title: "Appearance",
                        description: "\(name)'s appearance, edited from this "
                            + "window. Mode and color preview here while "
                            + "\(name) is active; transparency renders in "
                            + "\(name)'s own windows."
                    )
                    .padding(.bottom, 4)
                }

                Section {
                    Button("Use \(defaultWorkspaceLabel)'s appearance") {
                        revertToDefault()
                    }
                    .disabled(!hasOverrides)
                } header: {
                    SettingsSectionHeader(
                        title: "Inherits from \(defaultWorkspaceLabel)",
                        description: "\(name) uses \(defaultWorkspaceLabel)'s "
                            + "appearance until a setting below is changed. "
                            + "Revert drops \(name)'s own mode and "
                            + "transparency; its color stays."
                    )
                }

                Section {
                    Picker("Mode", selection: $mode) {
                        ForEach(ThemePreference.allCases) { preference in
                            Text(preference.title).tag(preference)
                        }
                    }
                    .pickerStyle(.segmented)
                    .font(.system(size: 13))
                    .foregroundStyle(Theme.foreground)
                } header: {
                    SettingsSectionHeader(
                        title: "Mode",
                        description: "Applies to \(name)'s window, sidebar and "
                            + "terminal colors."
                    )
                }

                Section {
                    HStack(spacing: 6) {
                        ForEach(AppTint.allCases) { candidate in
                            AppTintSwatch(tint: candidate, isSelected: tint == candidate) {
                                tint = candidate
                            }
                        }
                        Spacer()
                    }
                } header: {
                    SettingsSectionHeader(
                        title: "App color",
                        description: "Washes \(name)'s window chrome — and its "
                            + "dot in the workspace pickers."
                    )
                }

                Section {
                    TransparencySliderRow(
                        title: "Background",
                        value: $backgroundOpacity
                    )
                    TransparencySliderRow(
                        title: "Surface",
                        value: $surfaceOpacity
                    )
                } header: {
                    SettingsSectionHeader(
                        title: "Transparency",
                        description: "Applies to \(name)'s own windows — a "
                            + "running instance updates live."
                    )
                }
            }
            .formStyle(.grouped)
            .scrollContentBackground(.hidden)
        }
        .onAppear(perform: load)
        .onChange(of: mode) { newMode in
            guard loaded else { return }
            suite.set(newMode.rawValue, forKey: UnpeelStore.nativeThemeKey)
            notifyTarget()
            // This window follows the scope's saved mode.
            store.refreshScopeAppearance()
        }
        .onChange(of: tint) { newTint in
            guard loaded else { return }
            suite.set(newTint.rawValue, forKey: UnpeelStore.nativeAppTintKey)
            notifyTarget()
            // Repaint this window's chrome + the pickers' dots.
            store.applyScopeTint()
            NotificationCenter.default.post(
                name: .unpeelWorkspaceTintChanged, object: nil
            )
        }
        .onChange(of: backgroundOpacity) { _ in transparencyChanged() }
        .onChange(of: surfaceOpacity) { _ in transparencyChanged() }
    }

    private var defaultWorkspaceLabel: String {
        UnpeelWorkspaceContext.defaultWorkspaceName ?? "Personal"
    }

    /// Any own appearance value (mode or transparency) recorded in the
    /// workspace's suite — the revert button's enablement.
    private var hasOverrides: Bool {
        suite.string(forKey: UnpeelStore.nativeThemeKey) != nil
            || TransparencyModel.hasSavedValues(in: suite)
    }

    /// Decision 4's revert: drop the workspace's own mode + transparency so
    /// it inherits the default workspace's baseline again. Color stays —
    /// telling workspaces apart is its job.
    private func revertToDefault() {
        suite.removeObject(forKey: UnpeelStore.nativeThemeKey)
        TransparencyModel.clearSavedValues(in: suite)
        loaded = false
        load()
        notifyTarget()
        store.refreshScopeAppearance()
    }

    private func load() {
        mode = UnpeelStore.workspaceThemePreference(home: home)
        tint = UnpeelStore.workspaceTint(home: home)
        let values = TransparencyModel.savedValues(in: suite)
        backgroundOpacity = values.background
        surfaceOpacity = values.surface
        backgroundTone = values.backgroundTone
        surfaceTone = values.surfaceTone
        // Arm the writers only after the initial values settle, so loading
        // never writes the target suite.
        DispatchQueue.main.async { loaded = true }
    }

    private func transparencyChanged() {
        guard loaded else { return }
        TransparencyModel.write(
            background: backgroundOpacity,
            surface: surfaceOpacity,
            backgroundTone: backgroundTone,
            surfaceTone: surfaceTone,
            to: suite
        )
        notifyTarget()
    }

    /// Debounced `/reload-appearance` to the workspace's own ports, so a
    /// slider drag doesn't spray HTTP per tick.
    private func notifyTarget() {
        pingWorkItem?.cancel()
        let homeDir = URL(fileURLWithPath: home, isDirectory: true)
        let work = DispatchWorkItem {
            WorkspacesSettingsPanel.pingReloadAppearance(homeDir: homeDir)
        }
        pingWorkItem = work
        DispatchQueue.main.asyncAfter(deadline: .now() + 0.2, execute: work)
    }
}

/// Advanced ▸ Cleanup for the ACTIVE workspace, edited over the
/// `settings.workspace.set` verb — the same panel whether the workspace is a
/// loopback sibling on this Mac or an SSH/headless Host (Upstash-class).
/// Current values come from the bootstrap's additive `workspaceSettings`;
/// each change is one generation-bound effect and the post-effect snapshot
/// refresh reconciles the controls to what the Host actually holds.
private struct HostAdvancedSettingsPanel: View {
    @ObservedObject var store: UnpeelStore
    @ObservedObject var runtime: RemoteHostRuntime

    @State private var errorMessage: String?
    /// Optimistic picks, reconciled when the refreshed snapshot lands.
    @State private var draftMinutes: Int?
    @State private var draftLimit: Int?

    private var scopeName: String {
        store.selectedScopeDisplayName ?? "the selected workspace"
    }

    private var settings: RemoteWorkspaceSettings? {
        runtime.snapshot?.workspaceSettings
    }

    var body: some View {
        VStack(alignment: .leading, spacing: 0) {
            Form {
                Section {} header: {
                    SettingsPaneHeader(
                        title: "Advanced",
                        description: "\(scopeName)'s cleanup behavior, applied "
                            + "over the Host connection — the Host's own "
                            + "auto-archive sweep uses these; the app never "
                            + "archives on its own. Memory diagnostics are "
                            + "per-instance and live in each workspace's own "
                            + "window."
                    )
                    .padding(.bottom, 4)
                }

                if let settings {
                    Section {
                        Picker(
                            "Stop and archive idle sessions after",
                            selection: Binding(
                                get: { draftMinutes ?? settings.autoStopArchiveMinutes },
                                set: { minutes in
                                    draftMinutes = minutes
                                    apply(RemoteWorkspaceSettingsPatch(
                                        autoStopArchiveMinutes: minutes
                                    ))
                                }
                            )
                        ) {
                            ForEach(UnpeelStore.autoStopArchiveMinuteOptions, id: \.self) { minutes in
                                Text(UnpeelStore.autoStopArchiveLabel(for: minutes)).tag(minutes)
                            }
                        }

                        Picker(
                            "Stopped or archived sessions shown in sidebar",
                            selection: Binding(
                                get: { draftLimit ?? settings.sidebarStoppedLimit },
                                set: { limit in
                                    draftLimit = limit
                                    apply(RemoteWorkspaceSettingsPatch(
                                        sidebarStoppedLimit: limit
                                    ))
                                }
                            )
                        ) {
                            ForEach(UnpeelStore.sidebarStoppedLimitOptions, id: \.self) { limit in
                                Text(UnpeelStore.sidebarStoppedLimitLabel(for: limit)).tag(limit)
                            }
                        }
                    } header: {
                        SettingsSectionHeader(
                            title: "Cleanup",
                            description: "Idle sessions on \(scopeName) get the "
                                + "same treatment as Stop and archive; nothing "
                                + "is deleted. The sidebar limit hides older "
                                + "stopped or archived rows without removing them."
                        )
                    }
                } else {
                    Section {
                        Text("Waiting for \(scopeName)'s settings…")
                            .font(.system(size: 13))
                            .foregroundStyle(Theme.mutedForeground)
                    }
                }

                if let errorMessage {
                    Section {
                        Text(errorMessage)
                            .font(.system(size: 12))
                            .foregroundStyle(.red)
                    }
                }
            }
            .formStyle(.grouped)
            .scrollContentBackground(.hidden)
        }
        // Server truth arrived — drop the optimistic layer.
        .onChange(of: settings) { _ in
            draftMinutes = nil
            draftLimit = nil
        }
    }

    private func apply(_ patch: RemoteWorkspaceSettingsPatch) {
        errorMessage = nil
        Task { @MainActor in
            do {
                try await runtime.setWorkspaceSettings(patch)
            } catch {
                errorMessage = error.localizedDescription
            }
        }
    }
}

/// The MCP access policies for the ACTIVE workspace, edited over
/// `settings.workspace.set` — one panel for Browser use, Sessions use, and
/// Computer use while another workspace is active, identical for loopback
/// siblings and SSH/headless Hosts. Engine status, approvals lists, and
/// site rules remain in each workspace's own richer local panel for now.
private struct HostAccessSettingsPanel: View {
    @ObservedObject var store: UnpeelStore
    @ObservedObject var runtime: RemoteHostRuntime
    let tab: SettingsTab

    @State private var errorMessage: String?
    /// Optimistic per-control overrides, reconciled when the refreshed
    /// snapshot lands — controls respond on click, not on round-trip.
    @State private var stringOverrides: [String: String] = [:]
    @State private var boolOverrides: [String: Bool] = [:]

    private var scopeName: String {
        store.selectedScopeDisplayName ?? "the selected workspace"
    }

    private var settings: RemoteWorkspaceSettings? {
        runtime.snapshot?.workspaceSettings
    }

    var body: some View {
        VStack(alignment: .leading, spacing: 0) {
            Form {
                Section {} header: {
                    SettingsPaneHeader(
                        title: tab.title,
                        description: "\(scopeName)'s \(tab.title) policy, applied "
                            + "over the Host connection — its sessions read these "
                            + "per call, so changes apply live. Engine status, "
                            + "approvals, and site rules live in \(scopeName)'s "
                            + "own window for now."
                    )
                    .padding(.bottom, 4)
                }

                if let settings {
                    switch tab {
                    case .browser:
                        Section {
                            accessPicker(
                                "Browser access",
                                key: "browser",
                                value: settings.browserDefaultAccess,
                                options: [("on", "On"), ("ask", "Ask"), ("off", "Off")]
                            ) { value in
                                RemoteWorkspaceSettingsPatch(browserDefaultAccess: value)
                            }
                            Toggle(
                                "Browser screenshots go to gallery",
                                isOn: boolBinding(
                                    key: "screenshots",
                                    settings.mcpAutoAddBrowserScreenshots
                                ) { value in
                                    RemoteWorkspaceSettingsPatch(
                                        mcpAutoAddBrowserScreenshots: value
                                    )
                                }
                            )
                        }
                    case .sessions:
                        Section {
                            accessPicker(
                                "Writes to other sessions",
                                key: "write",
                                value: settings.mcpNonchildWriteAccess,
                                options: [("ask", "Ask"), ("allow", "Allow"), ("deny", "Deny")]
                            ) { value in
                                RemoteWorkspaceSettingsPatch(mcpNonchildWriteAccess: value)
                            }
                            Toggle(
                                "Agents may create worktrees",
                                isOn: boolBinding(key: "worktrees", settings.mcpWorktreeAccess) { value in
                                    RemoteWorkspaceSettingsPatch(mcpWorktreeAccess: value)
                                }
                            )
                        }
                    default:
                        Section {
                            if let experimental = settings.experimentalSettings {
                                LabeledContent("Host adapter") {
                                    Text(experimental.computerUseReady == true
                                        ? "Ready"
                                        : experimental.computerUseAvailable == true
                                            ? "Not running"
                                            : "Unavailable"
                                    )
                                    .foregroundStyle(
                                        experimental.computerUseReady == true
                                            ? Color.green : Theme.mutedForeground
                                    )
                                }
                                if let reason = experimental.computerUseUnavailableReason,
                                   !reason.isEmpty {
                                    Text(reason)
                                        .font(.system(size: 11))
                                        .foregroundStyle(Theme.mutedForeground)
                                        .fixedSize(horizontal: false, vertical: true)
                                }
                            }
                            accessPicker(
                                "Computer use",
                                key: "computer",
                                value: settings.computerAccess,
                                options: [("ask", "Ask"), ("allow", "Allow"), ("off", "Off")]
                            ) { value in
                                RemoteWorkspaceSettingsPatch(computerAccess: value)
                            }
                        }
                    }
                } else {
                    Section {
                        Text("Waiting for \(scopeName)'s settings…")
                            .font(.system(size: 13))
                            .foregroundStyle(Theme.mutedForeground)
                    }
                }

                if let errorMessage {
                    Section {
                        Text(errorMessage)
                            .font(.system(size: 12))
                            .foregroundStyle(.red)
                    }
                }
            }
            .formStyle(.grouped)
            .scrollContentBackground(.hidden)
        }
        // Server truth arrived — drop the optimistic layer.
        .onChange(of: settings) { _ in
            stringOverrides = [:]
            boolOverrides = [:]
        }
    }

    private func accessPicker(
        _ title: String,
        key: String,
        value: String,
        options: [(String, String)],
        patch: @escaping (String) -> RemoteWorkspaceSettingsPatch
    ) -> some View {
        Picker(
            title,
            selection: Binding(
                get: { stringOverrides[key] ?? value },
                set: { newValue in
                    stringOverrides[key] = newValue
                    apply(patch(newValue))
                }
            )
        ) {
            ForEach(options, id: \.0) { option in
                Text(option.1).tag(option.0)
            }
        }
        .pickerStyle(.segmented)
    }

    private func boolBinding(
        key: String,
        _ value: Bool,
        patch: @escaping (Bool) -> RemoteWorkspaceSettingsPatch
    ) -> Binding<Bool> {
        Binding(
            get: { boolOverrides[key] ?? value },
            set: { newValue in
                boolOverrides[key] = newValue
                apply(patch(newValue))
            }
        )
    }

    private func apply(_ patch: RemoteWorkspaceSettingsPatch) {
        errorMessage = nil
        Task { @MainActor in
            do {
                try await runtime.setWorkspaceSettings(patch)
            } catch {
                errorMessage = error.localizedDescription
            }
        }
    }
}

/// Transcript rendering for the ACTIVE workspace, edited over
/// `settings.workspace.set`'s nested transcript payload — the Host re-reads
/// `transcript_settings` per Markdown build, so changes apply to the next
/// copy or MCP read. Identical for loopback siblings and SSH Hosts.
private struct HostTranscriptsSettingsPanel: View {
    @ObservedObject var store: UnpeelStore
    @ObservedObject var runtime: RemoteHostRuntime

    @State private var errorMessage: String?
    /// Optimistic per-field overrides: the knob flips on click and the
    /// Host's refreshed snapshot reconciles (clearing these) when it lands —
    /// without this the toggles read dead for the refresh round-trip.
    @State private var overrides: [String: Bool] = [:]
    @State private var maxEntriesOverride: Int?

    private var scopeName: String {
        store.selectedScopeDisplayName ?? "the selected workspace"
    }

    private var settings: RemoteTranscriptSettings? {
        runtime.snapshot?.workspaceSettings?.transcriptSettings
    }

    // Scope rule: this is the
    // local Transcripts panel's exact layout — same rows, copy, and section
    // header — with only the write path swapped for the workspace verb.
    var body: some View {
        VStack(alignment: .leading, spacing: 0) {
            Form {
                Section {} header: {
                    SettingsPaneHeader(
                        title: "Transcripts",
                        description: "A session's conversation, rendered as Markdown — "
                            + "what \"Copy transcript\" copies and what agents read."
                    )
                    .padding(.bottom, 4)
                }

                if let settings {
                    transcriptSection(settings)
                } else {
                    Section {
                        Text("Waiting for \(scopeName)'s settings…")
                            .font(.system(size: 13))
                            .foregroundStyle(Theme.mutedForeground)
                    }
                }

                if let errorMessage {
                    Section {
                        Text(errorMessage)
                            .font(.system(size: 12))
                            .foregroundStyle(.red)
                    }
                }
            }
            .formStyle(.grouped)
            .scrollContentBackground(.hidden)
        }
        // Server truth arrived — drop the optimistic layer.
        .onChange(of: settings) { _ in
            overrides = [:]
            maxEntriesOverride = nil
        }
    }

    private func transcriptSection(_ settings: RemoteTranscriptSettings) -> some View {
        Section {
            transcriptToggle(
                title: "Session info header",
                subtitle: "Start with the session's title, ID, CLI, and model. "
                    + "The ID lets another agent target this session with the "
                    + "Sessions MCP tools.",
                key: "sessionInfo",
                on: settings.includeSessionInfo
            ) { RemoteTranscriptSettingsUpdate(includeSessionInfo: $0) }
            transcriptToggle(
                title: "User messages",
                key: "user",
                on: settings.includeUser
            ) { RemoteTranscriptSettingsUpdate(includeUser: $0) }
            transcriptToggle(
                title: "Assistant messages",
                key: "assistant",
                on: settings.includeAssistant
            ) { RemoteTranscriptSettingsUpdate(includeAssistant: $0) }
            transcriptToggle(
                title: "Reasoning",
                subtitle: "The agent's thinking blocks.",
                key: "reasoning",
                on: settings.includeReasoning
            ) { RemoteTranscriptSettingsUpdate(includeReasoning: $0) }
            transcriptToggle(
                title: "Tool calls & results",
                subtitle: "Commands the agent ran and their output.",
                key: "tools",
                on: settings.includeTools
            ) { RemoteTranscriptSettingsUpdate(includeTools: $0) }
            transcriptToggle(
                title: "File changes & diffs",
                key: "fileChanges",
                on: settings.includeFileChanges
            ) { RemoteTranscriptSettingsUpdate(includeFileChanges: $0) }
            transcriptToggle(
                title: "Plan updates",
                key: "planUpdates",
                on: settings.includePlanUpdates
            ) { RemoteTranscriptSettingsUpdate(includePlanUpdates: $0) }

            LabeledContent {
                Picker(
                    "",
                    selection: Binding(
                        get: { maxEntriesOverride ?? settings.maxEntries },
                        set: { value in
                            maxEntriesOverride = value
                            apply(RemoteWorkspaceSettingsPatch(
                                transcriptSettings: RemoteTranscriptSettingsUpdate(
                                    maxEntries: value
                                )
                            ))
                        }
                    )
                ) {
                    Text("Whole conversation").tag(0)
                    Text("Last 20 entries").tag(20)
                    Text("Last 50 entries").tag(50)
                    Text("Last 100 entries").tag(100)
                }
                .labelsHidden()
                .pickerStyle(.menu)
                .fixedSize()
            } label: {
                VStack(alignment: .leading, spacing: 1) {
                    Text("Range")
                        .font(.system(size: 13))
                        .foregroundStyle(Theme.foreground)
                    Text("How much of the conversation to include.")
                        .font(.system(size: 11))
                        .foregroundStyle(Theme.mutedForeground)
                        .fixedSize(horizontal: false, vertical: true)
                }
            }
        } header: {
            SettingsSectionHeader(
                title: "Transcript content",
                description: "What \"Copy transcript\" (right-click a session) puts on "
                    + "the clipboard as Markdown. These options also drive the defaults "
                    + "for agents reading a session's transcript. Range is the default "
                    + "for agent reads; the Copy transcript menu picks its own range."
            )
        }
    }

    private func transcriptToggle(
        title: String,
        subtitle: String? = nil,
        key: String,
        on: Bool,
        patch: @escaping (Bool) -> RemoteTranscriptSettingsUpdate
    ) -> some View {
        LabeledContent {
            Toggle(
                "",
                isOn: Binding(
                    get: { overrides[key] ?? on },
                    set: { newValue in
                        overrides[key] = newValue
                        apply(RemoteWorkspaceSettingsPatch(transcriptSettings: patch(newValue)))
                    }
                )
            )
            .toggleStyle(.switch)
            .labelsHidden()
            .controlSize(.small)
        } label: {
            VStack(alignment: .leading, spacing: 1) {
                Text(title)
                    .font(.system(size: 13))
                    .foregroundStyle(Theme.foreground)
                if let subtitle {
                    Text(subtitle)
                        .font(.system(size: 11))
                        .foregroundStyle(Theme.mutedForeground)
                        .fixedSize(horizontal: false, vertical: true)
                }
            }
        }
    }

    private func apply(_ patch: RemoteWorkspaceSettingsPatch) {
        errorMessage = nil
        Task { @MainActor in
            do {
                try await runtime.setWorkspaceSettings(patch)
            } catch {
                errorMessage = error.localizedDescription
            }
        }
    }
}

/// Notifications for the ACTIVE local workspace: its suite knob, written
/// cross-suite with a reload ping. With no own value the workspace inherits
/// the default workspace's setting (Decision 4 generalized) — the revert
/// section drops the override.
private struct HostNotificationsSettingsPanel: View {
    @ObservedObject var store: UnpeelStore
    let home: String
    let name: String

    @State private var menuAttention = true
    @State private var hasOverride = false
    @State private var loaded = false

    private var suite: UserDefaults { AppDefaults.suite(forUnpeelHome: home) }

    private var defaultWorkspaceLabel: String {
        UnpeelWorkspaceContext.defaultWorkspaceName ?? "Personal"
    }

    var body: some View {
        VStack(alignment: .leading, spacing: 0) {
            Form {
                Section {} header: {
                    SettingsPaneHeader(
                        title: "Notifications",
                        description: "How \(name) flags sessions that need you. "
                            + "Applies to \(name)'s own instance — live if it "
                            + "is running, otherwise from its next launch."
                    )
                    .padding(.bottom, 4)
                }

                Section {
                    Button("Use \(defaultWorkspaceLabel)'s notifications") {
                        revertToDefault()
                    }
                    .disabled(!hasOverride)
                } header: {
                    SettingsSectionHeader(
                        title: "Inherits from \(defaultWorkspaceLabel)",
                        description: "\(name) uses \(defaultWorkspaceLabel)'s "
                            + "notification settings until a setting below is "
                            + "changed. Revert drops \(name)'s own values."
                    )
                }

                Section {
                    LabeledContent {
                        Toggle(
                            "",
                            isOn: Binding(
                                get: { menuAttention },
                                set: { newValue in
                                    menuAttention = newValue
                                    guard loaded else { return }
                                    hasOverride = true
                                    suite.set(
                                        newValue,
                                        forKey: UnpeelStore.menuAttentionDetectionKey
                                    )
                                    WorkspacesSettingsPanel.pingReloadAppearance(
                                        homeDir: URL(fileURLWithPath: home, isDirectory: true)
                                    )
                                }
                            )
                        )
                        .toggleStyle(.switch)
                        .labelsHidden()
                        .controlSize(.small)
                    } label: {
                        VStack(alignment: .leading, spacing: 1) {
                            Text("Flag menus waiting for a choice")
                                .font(.system(size: 13))
                                .foregroundStyle(Theme.foreground)
                            Text("Show the yellow attention dot when an agent draws a "
                                + "pick-an-option menu. These prompts send no signal on "
                                + "their own, so Unpeel reads them off the screen.")
                                .font(.system(size: 11))
                                .foregroundStyle(Theme.mutedForeground)
                                .fixedSize(horizontal: false, vertical: true)
                        }
                    }
                } header: {
                    SettingsSectionHeader(
                        title: "Attention",
                        description: "When a session is waiting for you to answer an "
                            + "on-screen menu."
                    )
                }
            }
            .formStyle(.grouped)
            .scrollContentBackground(.hidden)
        }
        .onAppear(perform: load)
    }

    private func load() {
        let own = suite.object(forKey: UnpeelStore.menuAttentionDetectionKey) as? Bool
        hasOverride = own != nil
        // Own value → default workspace's (.standard baseline) → on.
        menuAttention = own
            ?? UserDefaults.standard.object(
                forKey: UnpeelStore.menuAttentionDetectionKey
            ) as? Bool
            ?? true
        DispatchQueue.main.async { loaded = true }
    }

    private func revertToDefault() {
        suite.removeObject(forKey: UnpeelStore.menuAttentionDetectionKey)
        loaded = false
        load()
        WorkspacesSettingsPanel.pingReloadAppearance(
            homeDir: URL(fileURLWithPath: home, isDirectory: true)
        )
    }
}

/// Experimental features for the ACTIVE local workspace: per-workspace
/// suite flags, written cross-suite. A running instance applies most flags
/// on its next UI evaluation; some fully land on its next launch.
private struct HostExperimentalSettingsPanel: View {
    @ObservedObject var store: UnpeelStore
    let home: String
    let name: String

    @State private var values: [String: Bool] = [:]
    @State private var hasOverride = false
    @State private var loaded = false

    private var suite: UserDefaults { AppDefaults.suite(forUnpeelHome: home) }

    private var features: [ExperimentalFeature] {
        ExperimentalFeature.all.filter { UnpeelFeatureFlags.isAvailable($0) }
    }

    private var defaultWorkspaceLabel: String {
        UnpeelWorkspaceContext.defaultWorkspaceName ?? "Personal"
    }

    var body: some View {
        VStack(alignment: .leading, spacing: 0) {
            Form {
                Section {} header: {
                    SettingsPaneHeader(
                        title: "Experimental",
                        description: "\(name)'s experimental features. Some "
                            + "changes fully apply when \(name) next launches."
                    )
                    .padding(.bottom, 4)
                }

                Section {
                    Button("Use \(defaultWorkspaceLabel)'s features") {
                        revertToDefault()
                    }
                    .disabled(!hasOverride)
                } header: {
                    SettingsSectionHeader(
                        title: "Inherits from \(defaultWorkspaceLabel)",
                        description: "\(name) uses \(defaultWorkspaceLabel)'s "
                            + "experimental features until a toggle below is "
                            + "changed. Revert drops \(name)'s own values."
                    )
                }

                Section {
                    ForEach(features) { feature in
                        featureRow(feature)
                    }
                }
            }
            .formStyle(.grouped)
            .scrollContentBackground(.hidden)
        }
        .onAppear(perform: load)
    }

    private func load() {
        hasOverride = features.contains {
            suite.object(forKey: $0.defaultsKey) != nil
        }
        for feature in features {
            values[feature.defaultsKey] = resolve(feature)
        }
        DispatchQueue.main.async { loaded = true }
    }

    /// Own value → default workspace's (.standard baseline) → built-in.
    private func resolve(_ feature: ExperimentalFeature) -> Bool {
        if let own = suite.object(forKey: feature.defaultsKey) as? Bool {
            return own
        }
        if let inherited = UserDefaults.standard.object(
            forKey: feature.defaultsKey
        ) as? Bool {
            return inherited
        }
        return feature.defaultOn
    }

    private func revertToDefault() {
        for feature in ExperimentalFeature.all {
            suite.removeObject(forKey: feature.defaultsKey)
        }
        loaded = false
        load()
        WorkspacesSettingsPanel.pingReloadAppearance(
            homeDir: URL(fileURLWithPath: home, isDirectory: true)
        )
    }

    /// Same row design as the local Experimental panel (Decision 6),
    /// writing the workspace's own suite instead of the store.
    private func featureRow(_ feature: ExperimentalFeature) -> some View {
        LabeledContent {
            Toggle("", isOn: Binding(
                get: { values[feature.defaultsKey] ?? resolve(feature) },
                set: { newValue in
                    values[feature.defaultsKey] = newValue
                    guard loaded else { return }
                    hasOverride = true
                    suite.set(newValue, forKey: feature.defaultsKey)
                    if feature == .computerUse {
                        // The Rust launch gate reads this workspace's
                        // app-state.json, not its defaults suite.
                        UnpeelStore.writeComputerUseExperiment(
                            newValue,
                            appStateFile: URL(fileURLWithPath: home, isDirectory: true)
                                .appendingPathComponent("app-state.json")
                        )
                    }
                    WorkspacesSettingsPanel.pingReloadAppearance(
                        homeDir: URL(fileURLWithPath: home, isDirectory: true)
                    )
                }
            ))
            .labelsHidden()
            .toggleStyle(.switch)
        } label: {
            VStack(alignment: .leading, spacing: 2) {
                Text(feature.title)
                    .font(.system(size: 13))
                    .foregroundStyle(Theme.foreground)
                Text(feature.summary)
                    .font(.system(size: 11))
                    .foregroundStyle(Theme.mutedForeground)
                    .lineSpacing(2)
                    .fixedSize(horizontal: false, vertical: true)
                    .frame(maxWidth: 460, alignment: .leading)
            }
        }
    }
}

/// Appearance for a true remote Host. The Host persists one presentation;
/// every Controller renders it while scoped there. Changes ride the same
/// generation-bound workspace-settings effect as the behavior panels.
private struct RemoteAppearanceSettingsPanel: View {
    @ObservedObject var store: UnpeelStore
    @ObservedObject var runtime: RemoteHostRuntime

    @State private var modeOverride: ThemePreference?
    @State private var tintOverride: AppTint?
    @State private var titleModeOverride: SessionTitleMode?
    @State private var backgroundOverride: Double?
    @State private var surfaceOverride: Double?
    @State private var backgroundToneOverride: Double?
    @State private var surfaceToneOverride: Double?
    @State private var transparencyWorkItem: DispatchWorkItem?
    @State private var errorMessage: String?

    private var scopeName: String {
        store.selectedScopeDisplayName ?? "the selected Host"
    }

    private var settings: RemoteAppearanceSettings? {
        runtime.snapshot?.workspaceSettings?.appearanceSettings
    }

    var body: some View {
        VStack(alignment: .leading, spacing: 0) {
            Form {
                Section {} header: {
                    SettingsPaneHeader(
                        title: "Appearance",
                        description: "How Controllers look while scoped to \(scopeName). "
                            + "The Host stores these values, so another Mac or phone "
                            + "uses the same workspace appearance."
                    )
                    .padding(.bottom, 4)
                }

                if let settings {
                    Section {
                        Picker(
                            "Mode",
                            selection: Binding(
                                get: {
                                    modeOverride
                                        ?? ThemePreference(rawValue: settings.theme)
                                        ?? .system
                                },
                                set: { value in
                                    modeOverride = value
                                    apply(RemoteAppearanceSettingsUpdate(
                                        theme: value.rawValue
                                    ))
                                }
                            )
                        ) {
                            ForEach(ThemePreference.allCases) { preference in
                                Text(preference.title).tag(preference)
                            }
                        }
                        .pickerStyle(.segmented)
                    } header: {
                        SettingsSectionHeader(
                            title: "Mode",
                            description: "Applies to this Controller's window, sidebar "
                                + "and terminal colors while \(scopeName) is active."
                        )
                    }

                    Section {
                        HStack(spacing: 6) {
                            ForEach(AppTint.allCases) { tint in
                                AppTintSwatch(
                                    tint: tint,
                                    isSelected: (tintOverride
                                        ?? AppTint(rawValue: settings.appTint)
                                        ?? .none) == tint
                                ) {
                                    tintOverride = tint
                                    apply(RemoteAppearanceSettingsUpdate(
                                        appTint: tint.rawValue
                                    ))
                                }
                            }
                            Spacer()
                        }
                    } header: {
                        SettingsSectionHeader(
                            title: "App color",
                            description: "Washes the Controller chrome and identifies "
                                + "\(scopeName) in workspace pickers."
                        )
                    }

                    Section {
                        Picker(
                            "Session titles",
                            selection: Binding(
                                get: {
                                    titleModeOverride
                                        ?? SessionTitleMode(
                                            rawValue: settings.sessionTitleMode
                                        )
                                        ?? .agent
                                },
                                set: { value in
                                    titleModeOverride = value
                                    apply(RemoteAppearanceSettingsUpdate(
                                        sessionTitleMode: value.rawValue
                                    ))
                                }
                            )
                        ) {
                            ForEach(SessionTitleMode.allCases) { mode in
                                Text(mode.title).tag(mode)
                            }
                        }
                        .pickerStyle(.menu)
                    } header: {
                        SettingsSectionHeader(
                            title: "Session titles",
                            description: "What names new sessions on the Host until you "
                                + "rename them. Running session hosts read this live."
                        )
                    }

                    Section {
                        OpenResourcesSettingsRows(runtime: runtime)
                    } header: {
                        SettingsSectionHeader(
                            title: "Open resources",
                            description: "Choose which Host-side App opens each supported type in this workspace."
                        )
                    }

                    Section {
                        TransparencySliderRow(
                            title: "Background",
                            value: Binding(
                                get: { backgroundOverride ?? settings.backgroundOpacity },
                                set: { value in
                                    backgroundOverride = value
                                    transparencyChanged(settings)
                                }
                            )
                        )
                        TransparencySliderRow(
                            title: "Surface",
                            value: Binding(
                                get: { surfaceOverride ?? settings.surfaceOpacity },
                                set: { value in
                                    surfaceOverride = value
                                    transparencyChanged(settings)
                                }
                            )
                        )
                        HStack {
                            Spacer()
                            Button("Revert to default") {
                                resetTransparency()
                            }
                            .controlSize(.small)
                        }
                    } header: {
                        SettingsSectionHeader(
                            title: "Transparency",
                            description: "Controls this Controller's background and "
                                + "terminal surface whenever \(scopeName) is selected."
                        )
                    }
                } else {
                    Section {
                        Text("Waiting for \(scopeName)'s appearance…")
                            .font(.system(size: 13))
                            .foregroundStyle(Theme.mutedForeground)
                    }
                }

                if let errorMessage {
                    Section {
                        Text(errorMessage)
                            .font(.system(size: 12))
                            .foregroundStyle(.red)
                    }
                }
            }
            .formStyle(.grouped)
            .scrollContentBackground(.hidden)
        }
        .onChange(of: settings) { _ in
            modeOverride = nil
            tintOverride = nil
            titleModeOverride = nil
            backgroundOverride = nil
            surfaceOverride = nil
            backgroundToneOverride = nil
            surfaceToneOverride = nil
        }
        .onDisappear { transparencyWorkItem?.cancel() }
    }

    private func currentTransparency(
        _ settings: RemoteAppearanceSettings
    ) -> (Double, Double, Double, Double) {
        (
            backgroundOverride ?? settings.backgroundOpacity,
            surfaceOverride ?? settings.surfaceOpacity,
            backgroundToneOverride ?? settings.backgroundTone,
            surfaceToneOverride ?? settings.surfaceTone
        )
    }

    private func transparencyChanged(_ settings: RemoteAppearanceSettings) {
        let values = currentTransparency(settings)
        // Preview locally without writing into this Controller workspace's
        // defaults. The Host effect is coalesced while a slider is dragged.
        TransparencyModel.shared.applyScopedPresentation(
            background: values.0,
            surface: values.1,
            backgroundTone: values.2,
            surfaceTone: values.3
        )
        transparencyWorkItem?.cancel()
        let work = DispatchWorkItem {
            apply(RemoteAppearanceSettingsUpdate(
                backgroundOpacity: values.0,
                surfaceOpacity: values.1,
                backgroundTone: values.2,
                surfaceTone: values.3
            ))
        }
        transparencyWorkItem = work
        DispatchQueue.main.asyncAfter(deadline: .now() + 0.2, execute: work)
    }

    private func resetTransparency() {
        transparencyWorkItem?.cancel()
        backgroundOverride = TransparencyModel.backgroundMaterialOpacity
        surfaceOverride = 1
        backgroundToneOverride = TransparencyModel.designBackgroundTone
        surfaceToneOverride = TransparencyModel.designSurfaceTone
        TransparencyModel.shared.applyScopedPresentation(
            background: TransparencyModel.backgroundMaterialOpacity,
            surface: 1,
            backgroundTone: TransparencyModel.designBackgroundTone,
            surfaceTone: TransparencyModel.designSurfaceTone
        )
        apply(RemoteAppearanceSettingsUpdate(
            backgroundOpacity: TransparencyModel.backgroundMaterialOpacity,
            surfaceOpacity: 1,
            backgroundTone: TransparencyModel.designBackgroundTone,
            surfaceTone: TransparencyModel.designSurfaceTone
        ))
    }

    private func apply(_ update: RemoteAppearanceSettingsUpdate) {
        errorMessage = nil
        Task { @MainActor in
            do {
                try await runtime.setWorkspaceSettings(RemoteWorkspaceSettingsPatch(
                    appearanceSettings: update
                ))
            } catch {
                errorMessage = error.localizedDescription
                modeOverride = nil
                tintOverride = nil
                titleModeOverride = nil
                backgroundOverride = nil
                surfaceOverride = nil
                backgroundToneOverride = nil
                surfaceToneOverride = nil
                store.refreshScopeAppearance()
                store.applyScopeTint()
            }
        }
    }
}

/// Host-backed attention behavior plus diagnostics for the Controller Mac.
/// Push delivery is capability-advertised; unsupported headless Hosts never
/// receive a fake local test action.
private struct RemoteNotificationsSettingsPanel: View {
    @ObservedObject var store: UnpeelStore
    @ObservedObject var runtime: RemoteHostRuntime
    @ObservedObject private var notifier = DesktopNotifier.shared

    @State private var menuAttentionOverride: Bool?
    @State private var errorMessage: String?

    private var scopeName: String {
        store.selectedScopeDisplayName ?? "the selected Host"
    }

    private var settings: RemoteNotificationSettings? {
        runtime.snapshot?.workspaceSettings?.notificationSettings
    }

    var body: some View {
        VStack(alignment: .leading, spacing: 0) {
            Form {
                Section {} header: {
                    SettingsPaneHeader(
                        title: "Notifications",
                        description: "How \(scopeName) flags sessions that need you, "
                            + "plus delivery diagnostics for this Controller Mac."
                    )
                    .padding(.bottom, 4)
                }

                if let settings {
                    Section {
                        LabeledContent {
                            Toggle(
                                "",
                                isOn: Binding(
                                    get: {
                                        menuAttentionOverride
                                            ?? settings.menuAttentionDetection
                                    },
                                    set: { value in
                                        menuAttentionOverride = value
                                        apply(RemoteNotificationSettingsUpdate(
                                            menuAttentionDetection: value
                                        ))
                                    }
                                )
                            )
                            .toggleStyle(.switch)
                            .labelsHidden()
                            .controlSize(.small)
                        } label: {
                            VStack(alignment: .leading, spacing: 1) {
                                Text("Flag menus waiting for a choice")
                                    .font(.system(size: 13))
                                    .foregroundStyle(Theme.foreground)
                                Text("Show the yellow attention dot when an agent draws "
                                    + "a pick-an-option menu on the Host.")
                                    .font(.system(size: 11))
                                    .foregroundStyle(Theme.mutedForeground)
                                    .fixedSize(horizontal: false, vertical: true)
                            }
                        }
                    } header: {
                        SettingsSectionHeader(
                            title: "Attention",
                            description: "Applied by the Host to its session activity "
                                + "before every Controller receives it."
                        )
                    }
                }

                Section {
                    Button(notifier.lastTestDiagnostic.isChecking
                        ? "Sending…"
                        : "Send a test notification on this Mac"
                    ) {
                        notifier.sendTestNotification()
                    }
                    .disabled(notifier.lastTestDiagnostic.isChecking)

                    SettingsValueRow(
                        label: "Last Mac test",
                        value: notifier.lastTestDiagnostic.label
                    )

                    if notifier.lastTestDiagnostic.needsSystemSettings {
                        Button("Open Mac Notification Settings…") {
                            if let url = URL(
                                string: "x-apple.systempreferences:com.apple.Notifications-Settings.extension"
                            ) {
                                NSWorkspace.shared.open(url)
                            }
                        }
                    }
                } header: {
                    SettingsSectionHeader(
                        title: "This Mac",
                        description: "Tests the notification banner on the Controller "
                            + "you are using now; it does not run anything on \(scopeName)."
                    )
                }

                Section {
                    SettingsValueRow(
                        label: "Phone registration",
                        value: runtime.supportsHostOperation(
                            RemoteHostRuntime.HostOperation.pushRegister
                        ) ? "Supported" : "Not advertised by this Host"
                    )
                    SettingsValueRow(
                        label: "Notify when done",
                        value: runtime.supportsHostOperation(
                            RemoteHostRuntime.HostOperation.notifyWhenDoneSet
                        ) ? "Available per session" : "Not advertised by this Host"
                    )
                } header: {
                    SettingsSectionHeader(
                        title: "Host delivery",
                        description: "Phone delivery is shown only when the Host "
                            + "advertises it. Upstash/Linux Hosts can still surface "
                            + "attention here without pretending to own an APNs path."
                    )
                }

                if let errorMessage {
                    Section {
                        Text(errorMessage)
                            .font(.system(size: 12))
                            .foregroundStyle(.red)
                    }
                }
            }
            .formStyle(.grouped)
            .scrollContentBackground(.hidden)
        }
        .onChange(of: settings) { _ in menuAttentionOverride = nil }
    }

    private func apply(_ update: RemoteNotificationSettingsUpdate) {
        errorMessage = nil
        Task { @MainActor in
            do {
                try await runtime.setWorkspaceSettings(RemoteWorkspaceSettingsPatch(
                    notificationSettings: update
                ))
            } catch {
                errorMessage = error.localizedDescription
                menuAttentionOverride = nil
            }
        }
    }
}

/// Experimental switches stored and enforced by the selected Host. The rows
/// stay data-driven from the native registry; the wire uses stable named
/// fields so unknown future additions remain additive.
private struct RemoteExperimentalSettingsPanel: View {
    @ObservedObject var store: UnpeelStore
    @ObservedObject var runtime: RemoteHostRuntime

    @State private var overrides: [String: Bool] = [:]
    @State private var errorMessage: String?

    private var scopeName: String {
        store.selectedScopeDisplayName ?? "the selected Host"
    }

    private var settings: RemoteExperimentalSettings? {
        runtime.snapshot?.workspaceSettings?.experimentalSettings
    }

    /// Computer use follows the selected Host, not this build (D2): the row
    /// shows when the Host advertises an adapter, and stays visible while
    /// the value is on so a Host that stops advertising can still be turned
    /// off (the one-way ratchet in `featureRow`).
    private var features: [ExperimentalFeature] {
        ExperimentalFeature.all.filter { feature in
            guard feature == .computerUse else { return UnpeelFeatureFlags.isAvailable(feature) }
            return UnpeelFeatureFlags.isAvailable(.computerUse)
                || UnpeelFeatureFlags.computerUseControllable(
                    hostAdvertisesAvailability: settings?.computerUseAvailable
                )
                || settings?.computerUse == true
        }
    }

    var body: some View {
        VStack(alignment: .leading, spacing: 0) {
            Form {
                Section {} header: {
                    SettingsPaneHeader(
                        title: "Experimental",
                        description: "Early features owned by \(scopeName). Session-tool "
                            + "changes apply to sessions started after the toggle."
                    )
                    .padding(.bottom, 4)
                }

                if let settings {
                    if features.isEmpty {
                        Section {
                            Text("No experimental features are available in this build.")
                                .font(.system(size: 12))
                                .foregroundStyle(Theme.mutedForeground)
                        }
                    } else {
                        Section {
                            ForEach(features) { feature in
                                featureRow(feature, settings: settings)
                            }
                        }
                    }
                } else {
                    Section {
                        Text("Waiting for \(scopeName)'s feature settings…")
                            .font(.system(size: 13))
                            .foregroundStyle(Theme.mutedForeground)
                    }
                }

                if let errorMessage {
                    Section {
                        Text(errorMessage)
                            .font(.system(size: 12))
                            .foregroundStyle(.red)
                    }
                }
            }
            .formStyle(.grouped)
            .scrollContentBackground(.hidden)
        }
        .onChange(of: settings) { _ in overrides = [:] }
    }

    private func featureRow(
        _ feature: ExperimentalFeature,
        settings: RemoteExperimentalSettings
    ) -> some View {
        let adapterAvailable = settings.computerUseAvailable == true
        let supported = feature != .computerUse || adapterAvailable
        let currentValue = overrides[feature.key] ?? value(feature, in: settings)
        // An unavailable Host must still let the user turn an already-on
        // value off; once off, it cannot be re-enabled until the Host
        // advertises a usable adapter.
        let canToggle = supported || (feature == .computerUse && currentValue)
        let detail: String = if feature == .computerUse, !supported {
            settings.computerUseUnavailableReason
                ?? "This Host does not advertise a Computer Use adapter. Update it and run "
                    + "`unpeel serve` from a graphical session."
        } else if feature == .computerUse,
                  settings.computerUse,
                  settings.computerUseReady != true {
            settings.computerUseUnavailableReason
                ?? "Cua Driver is starting on this Host. New sessions get Computer use once "
                    + "the adapter reports Ready."
        } else {
            feature.summary
        }
        return LabeledContent {
            Toggle(
                "",
                isOn: Binding(
                    get: { overrides[feature.key] ?? value(feature, in: settings) },
                    set: { enabled in
                        overrides[feature.key] = enabled
                        apply(update(feature, enabled: enabled))
                    }
                )
            )
            .labelsHidden()
            .toggleStyle(.switch)
            .disabled(!canToggle)
        } label: {
            VStack(alignment: .leading, spacing: 2) {
                Text(feature.title)
                    .font(.system(size: 13))
                    .foregroundStyle(Theme.foreground)
                Text(detail)
                .font(.system(size: 11))
                .foregroundStyle(Theme.mutedForeground)
                .lineSpacing(2)
                .fixedSize(horizontal: false, vertical: true)
                .frame(maxWidth: 460, alignment: .leading)
            }
        }
    }

    private func value(
        _ feature: ExperimentalFeature,
        in settings: RemoteExperimentalSettings
    ) -> Bool {
        if feature == .worktrees { return settings.worktrees }
        if feature == .sessionsMcp { return settings.sessionsMcp }
        if feature == .browserMcp { return settings.browserMcp }
        if feature == .computerUse { return settings.computerUse }
        if feature == .workspaces { return settings.workspaces }
        return false
    }

    private func update(
        _ feature: ExperimentalFeature,
        enabled: Bool
    ) -> RemoteExperimentalSettingsUpdate {
        if feature == .worktrees {
            return RemoteExperimentalSettingsUpdate(worktrees: enabled)
        }
        if feature == .sessionsMcp {
            return RemoteExperimentalSettingsUpdate(sessionsMcp: enabled)
        }
        if feature == .browserMcp {
            return RemoteExperimentalSettingsUpdate(browserMcp: enabled)
        }
        if feature == .computerUse {
            return RemoteExperimentalSettingsUpdate(computerUse: enabled)
        }
        if feature == .workspaces {
            return RemoteExperimentalSettingsUpdate(workspaces: enabled)
        }
        return RemoteExperimentalSettingsUpdate()
    }

    private func apply(_ update: RemoteExperimentalSettingsUpdate) {
        errorMessage = nil
        Task { @MainActor in
            do {
                try await runtime.setWorkspaceSettings(RemoteWorkspaceSettingsPatch(
                    experimentalSettings: update
                ))
            } catch {
                errorMessage = error.localizedDescription
                overrides = [:]
            }
        }
    }
}

/// Capability fallback for an older connected Host. Every visible remote
/// settings panel is implemented; reaching this view means the Host binary
/// predates the additive settings operation or bootstrap payload. Say that
/// directly instead of presenting the old "this panel is coming" scaffold.
private struct HostSettingsUpdateRequiredPanel: View {
    @ObservedObject var store: UnpeelStore
    @ObservedObject var runtime: RemoteHostRuntime
    let tab: SettingsTab

    private var scopeName: String {
        store.selectedScopeDisplayName ?? "the selected workspace"
    }

    private var protocolLabel: String? {
        guard let protocolDescriptor = runtime.snapshot?.hostProtocol else { return nil }
        return "Host protocol \(protocolDescriptor.majorVersion).\(protocolDescriptor.minorVersion)"
    }

    var body: some View {
        VStack(alignment: .leading, spacing: 0) {
            SettingsPaneHeader(
                title: tab.title,
                description: runtime.snapshot == nil
                    ? "Waiting for \(scopeName)'s settings and capabilities…"
                    : "\(scopeName) is running an older Unpeel Host that "
                        + "doesn't support remote \(tab.title) editing."
            )
            .padding(EdgeInsets(top: 20, leading: 20, bottom: 10, trailing: 20))

            if runtime.snapshot == nil {
                HStack(spacing: 8) {
                    ProgressView()
                        .controlSize(.small)
                    Text("Reconnecting to \(scopeName)…")
                }
                .font(.system(size: 13))
                .foregroundStyle(Theme.mutedForeground)
                .padding(.horizontal, 20)
            } else {
                VStack(alignment: .leading, spacing: 6) {
                    Text("Update Unpeel on \(scopeName), then reconnect this "
                        + "workspace. Its settings stay unchanged until the "
                        + "Host advertises the required capability.")
                    if let protocolLabel {
                        Text("\(protocolLabel) · needs "
                            + RemoteHostRuntime.HostOperation.workspaceSettingsSet)
                            .font(.system(size: 11, design: .monospaced))
                    }
                }
                .font(.system(size: 13))
                .foregroundStyle(Theme.mutedForeground)
                .padding(.horizontal, 20)
            }

            Spacer()
        }
        .frame(maxWidth: .infinity, maxHeight: .infinity, alignment: .topLeading)
    }
}

private struct SettingsScopePickerControl: View {
    @ObservedObject var store: UnpeelStore

    @State private var presented = false
    @State private var hovering = false
    @State private var activeRow: WorkspaceListRowModel?

    var body: some View {
        Button(action: present) {
            HStack(spacing: 8) {
                Circle()
                    .fill(Color(nsColor: (activeRow?.tint ?? .none).nsSwatch))
                    .frame(width: 8, height: 8)
                Text(activeName)
                    .font(Theme.rowLabelFont)
                    .foregroundStyle(Theme.foreground)
                    .lineLimit(1)
                Spacer(minLength: 4)
                if WorkspaceFeature.pickerEnabled {
                    Image(systemName: "chevron.up.chevron.down")
                        .font(.system(size: 9, weight: .semibold))
                        .foregroundStyle(Theme.mutedForeground)
                }
            }
            .padding(.horizontal, 10)
            .padding(.vertical, 7)
            .background(
                RoundedRectangle(cornerRadius: 7, style: .continuous)
                    .fill(hovering ? Theme.hoverRow : Theme.foreground.opacity(0.05))
            )
            .overlay(
                RoundedRectangle(cornerRadius: 7, style: .continuous)
                    .strokeBorder(Theme.resizerLine.opacity(0.5), lineWidth: 1)
            )
            .contentShape(Rectangle())
        }
        .buttonStyle(.plain)
        .disabled(!WorkspaceFeature.pickerEnabled)
        .onHover { hovering = WorkspaceFeature.pickerEnabled && $0 }
        .popover(isPresented: $presented, arrowEdge: .bottom) {
            WorkspacePickerPanel(
                store: store,
                hosts: store.remoteHostStore,
                pool: store.workspacePool,
                dismiss: { presented = false }
            )
        }
        .onAppear(perform: refreshActiveRow)
        .onChange(of: store.selectedHostScope) { _ in refreshActiveRow() }
        .onReceive(
            NotificationCenter.default.publisher(for: .unpeelWorkspaceTintChanged)
        ) { _ in refreshActiveRow() }
        .onReceive(
            NotificationCenter.default.publisher(for: .unpeelWorkspaceListChanged)
        ) { _ in refreshActiveRow() }
        .help("The active workspace — the workspace settings above the This Mac divider follow it")
    }

    private var activeName: String {
        activeRow?.name
            ?? store.selectedScopeDisplayName
            ?? UnpeelWorkspaceContext.advertisedHostName
    }

    private func present() {
        guard WorkspaceFeature.pickerEnabled else { return }
        // Same open-time freshness trigger as the sidebar dots' anchor.
        store.workspacePool.requestImmediateRefresh()
        presented = true
    }

    /// Row build reads registry/liveness/tints off disk — refreshed on
    /// appearance and change signals, never in a body evaluation.
    private func refreshActiveRow() {
        let rows = WorkspaceSwitching.orderedRows(store: store)
        activeRow = rows.first { WorkspaceSwitching.isScoped($0, store: store) }
    }
}

// MARK: - Content host (.settings-main-shell)

/// The content-pane half of settings: "Settings / <Tab>" titlebar over the
/// active panel. ContentArea swaps this with the terminal without any
/// animation (see TerminalArea.swift) — only the sidebar nav animates.
struct SettingsContentHost: View {
    @ObservedObject var store: UnpeelStore
    @ObservedObject private var transparency = TransparencyModel.shared

    /// Cached for the titlebar: the registry read behind it is disk IO, and
    /// this body re-runs on every store publish — never read files in a body
    /// evaluation (same rule as SidebarWorkspaceSelector). Refreshed on
    /// appearance and on scope changes; a rename from another instance lands
    /// the next time Settings opens.
    @State private var editedWorkspaceName: String?

    /// Collapsed sidebar: settings gets the same slide-down the terminal
    /// panes and full-content pages do — the breadcrumb becomes the compact
    /// title strip and the panel renders as a rounded card below it, frame
    /// material showing through around it.
    private var collapsedSurfaceCard: Bool { store.sidebarCollapsed }

    var body: some View {
        VStack(spacing: 0) {
            settingsTitlebar
            // Each panel is a grouped Form (its own scroll view), System
            // Settings style — no outer ScrollView. The column is capped at
            // 740pt: macOS grouped Form caps its section cards at ~700pt,
            // so 740 leaves the standard 20pt gutters and lets the panels'
            // fixed chrome (pane header, CTA rows) align with the cards by
            // using the same 20pt horizontal padding.
            panelContent
                .frame(maxWidth: 740)
                .frame(maxWidth: .infinity, maxHeight: .infinity)
                .mask(panelTopFade)
                // Collapsed-sidebar card chrome. Parameterized, never
                // structural, so toggling the sidebar can't remount the
                // panel (a Form would lose its scroll position).
                .background {
                    if collapsedSurfaceCard { SettingsMainBackground() }
                }
                .clipShape(
                    RoundedRectangle(
                        cornerRadius: collapsedSurfaceCard ? Theme.contentCornerRadius : 0,
                        style: .continuous
                    )
                )
                .overlay {
                    if collapsedSurfaceCard {
                        RoundedRectangle(
                            cornerRadius: Theme.contentCornerRadius,
                            style: .continuous
                        )
                        .strokeBorder(Theme.contentHairline, lineWidth: 1)
                        .allowsHitTesting(false)
                    }
                }
        }
        .background { hostBackdrop }
        // Same curve the workspace pane uses for its collapse slide-down,
        // so the strip compression, card chrome, and backdrop swap all move
        // with the sidebar. After the backdrop so the swap cross-fades.
        .animation(
            .timingCurve(0.25, 0.1, 0.25, 1, duration: 0.15),
            value: store.sidebarCollapsed
        )
        .background(
            // Escape closes settings (SettingsView.svelte handleKeydown).
            Button("") { store.closeSettings() }
                .keyboardShortcut(.cancelAction)
                .opacity(0)
        )
        .onAppear(perform: refreshEditedWorkspaceName)
        .onChange(of: store.selectedHostScope) { _ in
            refreshEditedWorkspaceName()
        }
        // Renames/removals from Settings ▸ Workspaces or the sidebar picker
        // land in the title immediately — the rename happens right under
        // this breadcrumb, so waiting for the next Settings open reads as
        // a stale-title bug.
        .onReceive(
            NotificationCenter.default.publisher(for: .unpeelWorkspaceListChanged)
        ) { _ in
            refreshEditedWorkspaceName()
        }
    }

    /// The window-frame backdrop behind the collapsed-sidebar card, the
    /// shared Surface paint otherwise. Translucent Surfaces paint nothing
    /// in card mode — the window-spanning frame backdrop already shows
    /// through (same rule as ContentArea's columnBackdrop).
    @ViewBuilder private var hostBackdrop: some View {
        if collapsedSurfaceCard {
            if transparency.surfaceOpacity < 1 {
                Color.clear
            } else {
                FrameBackdrop()
            }
        } else {
            SettingsMainBackground()
        }
    }

    /// "Settings / <Tab>" centered, 13px/600 muted, gap 8, separator at
    /// 0.54 opacity (SettingsView.svelte:300-324); the strip drags the
    /// window like the workspace titlebar. Collapsed sidebar compresses it
    /// to the terminal strip and centers the breadcrumb clear of the
    /// traffic lights and window buttons, riding up like the pane strip.
    private var settingsTitlebar: some View {
        ZStack {
            WindowDragArea()
            HStack(spacing: 8) {
                Text(titlebarScopeName.map { "Settings — \($0)" } ?? "Settings")
                Text("/").opacity(0.54)
                Text(selectedTab.title)
            }
            .font(.system(size: 13, weight: .semibold))
            .foregroundStyle(Theme.mutedForeground)
            // Share the content column's geometry (740pt cap + 20pt inset,
            // see panelContent) and left-align so the breadcrumb lines up
            // with the pane header below instead of centering on the window.
            .frame(
                maxWidth: .infinity,
                alignment: collapsedSurfaceCard ? .center : .leading
            )
            .padding(.horizontal, 20)
            .frame(maxWidth: 740)
            .frame(maxWidth: .infinity)
            .offset(y: collapsedSurfaceCard ? -4 : 0)
            .allowsHitTesting(false)
        }
        .frame(height: collapsedSurfaceCard ? Theme.titleStripHeight : Theme.titlebarHeight)
    }

    /// The breadcrumb is the only pinned chrome; the pane title, description
    /// and every section scroll under it. This top fade dissolves that content
    /// as it meets the breadcrumb instead of cutting it off with a hard edge.
    private var panelTopFade: some View {
        VStack(spacing: 0) {
            LinearGradient(colors: [.clear, .black], startPoint: .top, endPoint: .bottom)
                .frame(height: 22)
            Color.black
        }
    }

    @ViewBuilder
    private var panelContent: some View {
        // Every tab except the machine-level Workspaces registry belongs to
        // the ACTIVE workspace. While another workspace is active, only tabs
        // with Host settings verbs can actually edit it; the rest say so
        // honestly instead of silently editing this instance.
        if store.selectedHostScope != .local, selectedTab != .workspaces {
            if selectedTab == .presets {
                // Preset editing uses the Host's preset verbs.
                HostPresetsSettingsPanel(store: store, runtime: store.remoteHostRuntime)
            } else if selectedTab == .mobile {
                // Pairing follows the scope (one pairing = one workspace);
                // the panel itself branches on the selected scope kind.
                RemoteSettingsPanel(store: store)
            } else if selectedTab == .advanced,
                      store.remoteHostRuntime.supportsHostOperation(
                          RemoteHostRuntime.HostOperation.workspaceSettingsSet
                      ) {
                // Behavior knobs ride the settings verb — one path for a
                // loopback workspace and an SSH/headless Host alike.
                HostAdvancedSettingsPanel(store: store, runtime: store.remoteHostRuntime)
            } else if selectedTab == .transcripts,
                      store.remoteHostRuntime.supportsHostOperation(
                          RemoteHostRuntime.HostOperation.workspaceSettingsSet
                      ),
                      store.remoteHostRuntime.snapshot?.workspaceSettings?
                          .transcriptSettings != nil {
                // Transcript rendering rides the settings verb too; the
                // nested payload is additive, so a Host that predates it
                // falls through to the placeholder.
                HostTranscriptsSettingsPanel(store: store, runtime: store.remoteHostRuntime)
            } else if [.browser, .sessions, .computer].contains(selectedTab),
                      store.remoteHostRuntime.supportsHostOperation(
                          RemoteHostRuntime.HostOperation.workspaceSettingsSet
                      ) {
                // The access policies ride the same settings verb — one
                // path for a loopback workspace and an SSH/headless Host.
                HostAccessSettingsPanel(
                    store: store,
                    runtime: store.remoteHostRuntime,
                    tab: selectedTab
                )
            } else if selectedTab == .appearance,
                      case let .localWorkspace(home, name) = store.selectedHostScope {
                // A LOCAL workspace's settings are reachable as files on
                // this Mac — same cross-suite write + reload ping the
                // Workspaces tab's color pickers use. No Host verb needed;
                // remote Hosts still get the unavailable panel.
                HostAppearanceSettingsPanel(store: store, home: home, name: name)
            } else if selectedTab == .appearance,
                      store.remoteHostRuntime.supportsHostOperation(
                          RemoteHostRuntime.HostOperation.workspaceSettingsSet
                      ),
                      store.remoteHostRuntime.snapshot?.workspaceSettings?
                          .appearanceSettings != nil {
                RemoteAppearanceSettingsPanel(
                    store: store,
                    runtime: store.remoteHostRuntime
                )
            } else if selectedTab == .notifications,
                      case let .localWorkspace(home, name) = store.selectedHostScope {
                HostNotificationsSettingsPanel(store: store, home: home, name: name)
            } else if selectedTab == .notifications,
                      store.remoteHostRuntime.supportsHostOperation(
                          RemoteHostRuntime.HostOperation.workspaceSettingsSet
                      ),
                      store.remoteHostRuntime.snapshot?.workspaceSettings?
                          .notificationSettings != nil {
                RemoteNotificationsSettingsPanel(
                    store: store,
                    runtime: store.remoteHostRuntime
                )
            } else if selectedTab == .experimental,
                      case let .localWorkspace(home, name) = store.selectedHostScope {
                HostExperimentalSettingsPanel(store: store, home: home, name: name)
            } else if selectedTab == .experimental,
                      store.remoteHostRuntime.supportsHostOperation(
                          RemoteHostRuntime.HostOperation.workspaceSettingsSet
                      ),
                      store.remoteHostRuntime.snapshot?.workspaceSettings?
                          .experimentalSettings != nil {
                RemoteExperimentalSettingsPanel(
                    store: store,
                    runtime: store.remoteHostRuntime
                )
            } else if selectedTab == .worktrees {
                // Listing and local-machine git verbs use the scoped
                // project tree; there is no Host worktree-management verb.
                WorktreesSettingsPanel(store: store)
            } else {
                HostSettingsUpdateRequiredPanel(
                    store: store,
                    runtime: store.remoteHostRuntime,
                    tab: selectedTab
                )
            }
        } else {
            localPanel
        }
    }

    @ViewBuilder
    private var localPanel: some View {
        switch selectedTab {
        case .appearance:
            AppearanceSettingsPanel(store: store)
        case .presets:
            PresetsSettingsPanel(store: store)
        case .transcripts:
            TranscriptsSettingsPanel(store: store)
        case .notifications:
            NotificationsSettingsPanel(store: store)
        case .sessions:
            UnpeelMCPSettingsPanel(store: store)
        case .browser:
            BrowserSettingsPanel(store: store)
        case .computer:
            ComputerSettingsPanel(store: store)
        case .experimental:
            ExperimentalSettingsPanel(store: store)
        case .mobile:
            RemoteSettingsPanel(store: store)
        case .workspaces:
            // Unpeel Link (license + enrollment) lives on the Remote tab.
            WorkspacesSettingsPanel(
                store: store,
                onOpenPro: { store.openSettings(tab: .mobile) }
            )
        case .worktrees:
            WorktreesSettingsPanel(store: store)
        case .advanced:
            AdvancedSettingsPanel(store: store)
        }
    }

    private var selectedTab: SettingsTab {
        if SettingsTab.visibleCases(computerUseControllable: store.selectedHostAdvertisesComputerUse).contains(store.settingsTab) {
            return store.settingsTab
        }
        // The selected tab's gate (Mobile dev flag, Sessions MCP experiment)
        // turned off — fall back to the first tab. Workspaces leads the enum
        // but is itself gated, so resolve through visibleCases.
        return SettingsTab.visibleCases(computerUseControllable: store.selectedHostAdvertisesComputerUse).first ?? .presets
    }

    /// The titlebar names the workspace the visible panel belongs to: the
    /// active workspace for every tab except the machine-level Workspaces
    /// registry (which is this Mac's, whatever is scoped).
    private var titlebarScopeName: String? {
        if store.selectedHostScope != .local, selectedTab != .workspaces {
            return store.selectedScopeDisplayName
        }
        return editedWorkspaceName
    }

    /// Interim scope labeling (scope
    /// rule): Settings always edits THIS instance's workspace, even
    /// while the sidebar picker scopes the window to another Host. Name the
    /// edited workspace whenever another scope exists to be confused with;
    /// a plain single-workspace install keeps the bare "Settings" title.
    private func refreshEditedWorkspaceName() {
        if let name = UnpeelWorkspaceContext.displayName {
            editedWorkspaceName = name
            return
        }
        let otherScopesExist = store.selectedHostScope != .local
            || !UnpeelWorkspaceRegistry.load().isEmpty
            || !store.remoteHostStore.records.isEmpty
        editedWorkspaceName =
            otherScopesExist ? UnpeelWorkspaceContext.advertisedHostName : nil
    }
}

/// `.settings-main-shell` background: the glass content tint layered over
/// an extra dimming wash — black 24% dark, white 36% light
/// (SettingsView.svelte:273-282 + the light-theme override below it).
struct SettingsMainBackground: View {
    var body: some View {
        // Settings shares the ONE Surface backdrop with the terminal and
        // every other main page (same canvas color, same opacity) — the
        // historical shell dim made settings a visibly different background.
        // No ignoresSafeArea: inside the inset content pane it would extend
        // through the titlebar safe area to the window top (see
        // ContentBackground).
        SurfaceBackdrop()
    }
}

// MARK: - Appearance panel (settings/AppearancePanel.svelte, mode only)

private struct OpenResourcesSettingsRows: View {
    @ObservedObject var runtime: RemoteHostRuntime
    @State private var overrides: [String: String] = [:]
    @State private var installing: Set<String> = []
    @State private var errorMessage: String?

    private var apps: [RemoteAppSummary] {
        runtime.snapshot?.availableApps ?? []
    }

    private var installedIDs: Set<String> {
        Set((runtime.snapshot?.installedApps ?? []).map(\.id))
            .union(apps.filter { $0.installed }.map(\.id))
    }

    private var selectors: [String] {
        let fileSelectors = apps.flatMap(\.mediaTypes).map { "file:\($0)" }
        let resourceSelectors = apps.flatMap(\.resourceKinds).map { "resource:\($0)" }
        return Array(Set(fileSelectors + resourceSelectors)).sorted {
            selectorTitle($0) < selectorTitle($1)
        }
    }

    var body: some View {
        ForEach(selectors, id: \.self) { selector in
            HStack(spacing: 10) {
                Text(selectorTitle(selector))
                Spacer()
                Picker("", selection: selection(for: selector)) {
                    ForEach(apps.filter { $0.handles(selector: selector) }) { app in
                        Text(app.name + (installedIDs.contains(app.id) ? "" : " (Not installed)"))
                            .tag("app:\(app.id)")
                    }
                    if selector.hasPrefix("file:") {
                        Divider()
                        Text("Default Editor").tag("editor")
                        Text("System Default").tag("system")
                    }
                }
                .labelsHidden()
                .frame(width: 190)

                if let app = selectedMissingApp(for: selector) {
                    Button {
                        install(app)
                    } label: {
                        if installing.contains(app.id) {
                            ProgressView().controlSize(.small)
                        } else {
                            Text("Install")
                        }
                    }
                    .controlSize(.small)
                    .disabled(installing.contains(app.id))
                }
            }
        }
        if let errorMessage {
            Text(errorMessage)
                .font(.system(size: 11))
                .foregroundStyle(.red)
        }
    }

    private func selection(for selector: String) -> Binding<String> {
        Binding(
            get: {
                overrides[selector]
                    ?? runtime.snapshot?.openers?[selector]
                    ?? apps.first(where: {
                        $0.handles(selector: selector) && $0.defaultFor.contains(selector)
                    })
                        .map { "app:\($0.id)" }
                    ?? singleHandler(for: selector)
                        .map { "app:\($0.id)" }
                    ?? (selector.hasPrefix("file:") ? "editor" : "")
            },
            set: { opener in
                overrides[selector] = opener
                errorMessage = nil
                Task { @MainActor in
                    do {
                        try await runtime.setOpener(selector: selector, opener: opener)
                    } catch {
                        overrides.removeValue(forKey: selector)
                        errorMessage = error.localizedDescription
                    }
                }
            }
        )
    }

    private func singleHandler(for selector: String) -> RemoteAppSummary? {
        let matches = apps.filter { $0.handles(selector: selector) }
        return matches.count == 1 ? matches[0] : nil
    }

    private func selectedMissingApp(for selector: String) -> RemoteAppSummary? {
        let opener = selection(for: selector).wrappedValue
        guard opener.hasPrefix("app:") else { return nil }
        let appID = String(opener.dropFirst(4))
        guard !installedIDs.contains(appID) else {
            return nil
        }
        return apps.first { $0.id == appID }
    }

    private func install(_ app: RemoteAppSummary) {
        installing.insert(app.id)
        errorMessage = nil
        Task { @MainActor in
            defer { installing.remove(app.id) }
            do {
                try await runtime.installApp(app.id)
            } catch {
                errorMessage = error.localizedDescription
            }
        }
    }

    private func selectorTitle(_ selector: String) -> String {
        switch selector {
        case "file:text/markdown": "Markdown"
        case "file:text/html": "HTML"
        case "file:text/csv": "CSV"
        case "resource:folder": "Folders"
        case "resource:git.working-tree": "Git changes"
        case "resource:github.repository": "GitHub repositories"
        default:
            selector
                .replacingOccurrences(of: "file:", with: "")
                .replacingOccurrences(of: "resource:", with: "")
        }
    }
}

/// Native Appearance panel: the theme mode picker. The Svelte panel's
/// second control (Ambience color schemes) has no native machinery yet, so
/// it is omitted rather than faked.
struct AppearanceSettingsPanel: View {
    @ObservedObject var store: UnpeelStore
    @ObservedObject private var transparency = TransparencyModel.shared

    /// The same editor set the titlebar "open" dropdown offers, limited to
    /// installed apps — plus the current selection even if it isn't installed,
    /// so a saved default never silently disappears from the picker.
    private var editorOptions: [WorkspaceOpenTarget] {
        var options = WorkspaceOpenTarget.editorTargets.filter { $0.isAvailable }
        if !options.contains(where: { $0.codeEditorID == store.codeEditor }),
           let selected = WorkspaceOpenTarget.editorTargets
               .first(where: { $0.codeEditorID == store.codeEditor }) {
            options.insert(selected, at: 0)
        }
        return options
    }

    var body: some View {
        VStack(alignment: .leading, spacing: 0) {
            Form {
                // Pane title/description ride along as a background-less
                // Section header so they scroll under the sticky breadcrumb,
                // unlike grouped-Form rows which always grow a card.
                Section {} header: {
                    SettingsPaneHeader(
                        title: "Appearance",
                        description: "How Unpeel looks. System follows your macOS appearance."
                    )
                    .padding(.bottom, 4)
                }

                // Decision 4: a workspace instance inherits the default
                // workspace's appearance until it sets its own; offer the
                // revert right here.
                if !UnpeelWorkspaceContext.isDefaultInstance {
                    Section {
                        Button(
                            "Use \(UnpeelWorkspaceContext.defaultWorkspaceName ?? "Personal")'s appearance"
                        ) {
                            store.revertAppearanceToInheritedBaseline()
                        }
                    } header: {
                        SettingsSectionHeader(
                            title: "Inherits from \(UnpeelWorkspaceContext.defaultWorkspaceName ?? "Personal")",
                            description: "This workspace uses the default "
                                + "workspace's appearance until a setting "
                                + "below is changed. Revert drops its own "
                                + "mode and transparency; its color stays."
                        )
                    }
                }

                Section {
                    Picker("Mode", selection: Binding(
                        get: { store.themePreference },
                        set: { store.setThemePreference($0) }
                    )) {
                        ForEach(ThemePreference.allCases) { preference in
                            Text(preference.title).tag(preference)
                        }
                    }
                    .pickerStyle(.segmented)
                    .font(.system(size: 13))
                    .foregroundStyle(Theme.foreground)
                } header: {
                    SettingsSectionHeader(
                        title: "Mode",
                        description: "Applies to the window, sidebar and terminal colors. "
                            + "Claude Code has its own theme setting — run /config inside "
                            + "Claude Code and change Theme to match."
                    )
                }

                Section {
                    HStack(spacing: 6) {
                        ForEach(AppTint.allCases) { tint in
                            AppTintSwatch(tint: tint, isSelected: store.appTint == tint) {
                                store.setAppTint(tint)
                            }
                        }
                        Spacer()
                    }
                } header: {
                    SettingsSectionHeader(
                        title: "App color",
                        description: "Washes this workspace's window chrome — sidebar, "
                            + "content, and terminal canvas. Each workspace keeps its "
                            + "own color (also editable per workspace in Settings ▸ "
                            + "Workspaces)."
                    )
                }

                Section {
                    Picker("Session titles", selection: Binding(
                        get: { store.sessionTitleMode },
                        set: { store.setSessionTitleMode($0) }
                    )) {
                        ForEach(SessionTitleMode.allCases) { mode in
                            Text(mode.title).tag(mode)
                        }
                    }
                    .pickerStyle(.menu)
                    .font(.system(size: 13))
                    .foregroundStyle(Theme.foreground)
                } header: {
                    SettingsSectionHeader(
                        title: "Session titles",
                        description: "What names a session in the sidebar until you "
                            + "rename it. First prompt titles it once from your first "
                            + "message. Live from agent follows the agent's own task "
                            + "summary as it works (agents that publish one — Claude "
                            + "today), falling back to the first prompt until it "
                            + "appears. Renaming a session always wins."
                    )
                }

                Section {
                    TransparencySliderRow(
                        title: "Background",
                        value: $transparency.backgroundOpacity
                    )
                    TransparencySliderRow(
                        title: "Surface",
                        value: $transparency.surfaceOpacity
                    )
                    HStack {
                        Spacer()
                        Button("Revert to default") {
                            transparency.resetToDefaults()
                        }
                        .controlSize(.small)
                        .disabled(transparency.isDefault)
                    }
                } header: {
                    SettingsSectionHeader(
                        title: "Transparency",
                        description: "Background is the window backdrop — the sidebar "
                            + "and everything behind the content; below 100% the "
                            + "desktop shows through it, natively blurred. Surface "
                            + "covers the terminal canvas, settings, and the other "
                            + "pages on top of it. 100% is fully opaque. Terminal "
                            + "text always stays fully opaque."
                    )
                }

                Section {
                    Picker("Editor", selection: Binding(
                        get: { store.codeEditor },
                        set: { store.setCodeEditor($0) }
                    )) {
                        ForEach(editorOptions, id: \.id) { target in
                            Label {
                                Text(target.title)
                            } icon: {
                                WorkspaceAppIconView(target: target, size: 16)
                            }
                            .tag(target.codeEditorID ?? "")
                        }
                    }
                    .pickerStyle(.menu)
                    .font(.system(size: 13))
                    .foregroundStyle(Theme.foreground)

                    OpenResourcesSettingsRows(runtime: store.remoteHostRuntime)
                } header: {
                    SettingsSectionHeader(
                        title: "Open resources",
                        description: "Choose what opens each supported type in this workspace. The editor is "
                            + "also used by \"Open in editor\" and the titlebar open button."
                    )
                }

                Section {
                    Picker("⌘T", selection: $store.commandTAction) {
                        ForEach(CommandTAction.allCases) { action in
                            Text(action.title).tag(action)
                        }
                    }
                    .pickerStyle(.segmented)
                    .font(.system(size: 13))
                    .foregroundStyle(Theme.foreground)

                    LabeledContent {
                        Toggle(
                            "",
                            isOn: Binding(
                                get: { store.showSessionGallery },
                                set: { store.showSessionGallery = $0 }
                            )
                        )
                        .toggleStyle(.switch)
                        .labelsHidden()
                        .controlSize(.small)
                    } label: {
                        VStack(alignment: .leading, spacing: 1) {
                            Text("Session gallery")
                                .font(.system(size: 13))
                                .foregroundStyle(Theme.foreground)
                            Text("Photo chip in the terminal title bar with the "
                                + "session's captures, plus Take Screenshot (⇧⌘S) "
                                + "to shoot into the session and attach it to the "
                                + "prompt. Turn off if you use your own screenshot "
                                + "tools.")
                                .font(.system(size: 11))
                                .foregroundStyle(Theme.mutedForeground)
                                .fixedSize(horizontal: false, vertical: true)
                        }
                    }
                } header: {
                    SettingsSectionHeader(
                        title: "Terminal",
                        description: "Choose what ⌘T opens and configure extras around "
                            + "the terminal view."
                    )
                }
            }
            .formStyle(.grouped)
            .scrollContentBackground(.hidden)
        }
    }
}

/// One opacity/tone slider row: label, slider, and a fixed-width live
/// percentage so the row doesn't wiggle while dragging. When the value sits
/// on `autoValue` (a design detent) the readout says "Auto" instead.
struct TransparencySliderRow: View {
    let title: String
    @Binding var value: Double
    var range = TransparencyModel.opacityRange
    var step = TransparencyModel.opacityStep
    var autoValue: Double? = nil

    private var isAuto: Bool {
        guard let autoValue else { return false }
        return abs(value - autoValue) < 0.001
    }

    var body: some View {
        LabeledContent {
            HStack(spacing: 10) {
                Slider(value: $value, in: range, step: step)
                    .controlSize(.small)
                    .frame(width: 180)
                Text(isAuto ? "Auto" : "\(Int((value * 100).rounded()))%")
                    .font(.system(size: 12).monospacedDigit())
                    .foregroundStyle(Theme.mutedForeground)
                    .frame(width: 38, alignment: .trailing)
            }
        } label: {
            Text(title)
                .font(.system(size: 13))
                .foregroundStyle(Theme.foreground)
        }
    }
}

/// One App-color chip: the tint's representative color with a selection ring.
struct AppTintSwatch: View {
    let tint: AppTint
    let isSelected: Bool
    let action: () -> Void

    var body: some View {
        Button(action: action) {
            ZStack {
                Circle()
                    .fill(tint.swatch)
                    .frame(width: 20, height: 20)
                Circle()
                    .strokeBorder(
                        isSelected ? Theme.foreground : Color.clear,
                        lineWidth: 2
                    )
                    .frame(width: 27, height: 27)
            }
            .frame(width: 28, height: 28)
        }
        .buttonStyle(.plain)
        .help(tint.title)
        .accessibilityLabel(Text(tint.title))
    }
}

// MARK: - Remote access panel

struct RemoteSettingsPanel: View {
    @ObservedObject var store: UnpeelStore
    @ObservedObject private var management: HostManagementState

    init(store: UnpeelStore) {
        self.store = store
        self.management = store.hostManagement
    }

    /// Install link for the iPhone app. Always unpeel.com/ios — the site 302s
    /// to the current TestFlight public link (and later the App Store page),
    /// so shipped desktop builds never hold a stale store URL.
    private static let iosAppURL = URL(string: "https://unpeel.com/ios")!

    /// The default workspace is implicit; every registry record is an
    /// additional workspace on this Mac. Keep the friendlier machine copy on
    /// a one-workspace install, but name the sharing boundary once that could
    /// otherwise be ambiguous.
    private var hasMultipleLocalWorkspaces: Bool {
        !UnpeelWorkspaceRegistry.load().isEmpty
    }

    /// Pairing follows the scope (one pairing = one workspace, 2026-08-23):
    /// this tab is per-workspace and context-switches on the selected scope.
    /// The active workspace gets its own full panel (pairing server, Link,
    /// license); another LOCAL workspace gets its pairing section (the
    /// sibling instance mints the code over the loopback bridge); a remote
    /// Host gets the sealed invitation flow when it advertises the
    /// capability.
    private var remoteDescription: String {
        if let scope = scopedLocalWorkspace {
            return "Let another Unpeel device control \(scope.name). Devices "
                + "you pair here reach \(scope.name) only — each workspace "
                + "pairs its own devices."
        }
        if let scopeName = store.selectedScopeDisplayName {
            return "Let another Unpeel device control \(scopeName). "
                + "\(scopeName) mints the credentials; this Mac only forwards "
                + "the one-time sealed exchange."
        }
        if hasMultipleLocalWorkspaces {
            return "Let another Unpeel device control this workspace."
        }
        return "Let another Unpeel device control this Mac."
    }

    /// The selected LOCAL workspace scope, if any — the target of the
    /// scoped pairing section.
    private var scopedLocalWorkspace: (home: String, name: String)? {
        guard case let .localWorkspace(home, name) = store.selectedHostScope else {
            return nil
        }
        return (home, name)
    }

    @State private var sharePresented = false
    @State private var workspaceSharePresented = false
    @State private var remoteHostSharePresented = false
    @State private var scopedDevices: [ScopedPairedDevice] = []

    var body: some View {
        VStack(alignment: .leading, spacing: 0) {
            Form {
                Section {} header: {
                    SettingsPaneHeader(
                        title: "Remote Control",
                        description: remoteDescription
                    )
                    .padding(.bottom, 4)
                }

                if let scope = scopedLocalWorkspace {
                    scopedWorkspaceSection(scope)
                    testflightSection
                    scopedLinkFootnote(scope)
                } else if store.selectedHostScope.remoteHostID != nil {
                    remoteHostSection
                    testflightSection
                } else {
                    controlsThisMacSection
                    testflightSection
                    // Unpeel Link: the enrollment list (which replaced the
                    // old global relay toggle — the uplink runs whenever ≥1
                    // inbound device is on Link), then the seat block.
                    LinkEnrollmentSection(
                        store: store,
                        hosts: store.remoteHostStore,
                        usesWorkspaceLanguage: hasMultipleLocalWorkspaces
                    )
                    LinkLicenseSections(store: store)
                    securitySection
                }
            }
            .formStyle(.grouped)
            .scrollContentBackground(.hidden)
        }
        .onAppear {
            store.refreshPairedControllers()
            if let scope = scopedLocalWorkspace {
                loadScopedDevices(home: scope.home)
            }
        }
        .onChange(of: scopedLocalWorkspace?.home) { home in
            scopedDevices = []
            if let home { loadScopedDevices(home: home) }
        }
    }

    // MARK: - Selected-workspace pairing (one pairing = one workspace)

    /// Read-only projection of the scoped workspace's own paired-device
    /// list (`<home>/mobile/devices.json`, same-user disk read). Display
    /// only: revocation stays in that workspace's own instance, which owns
    /// the file and its cached credential state.
    struct ScopedPairedDevice: Decodable, Identifiable {
        let id: String
        let name: String
        let platform: String?
    }

    private struct ScopedPairedDeviceFile: Decodable {
        let devices: [ScopedPairedDevice]
    }

    private func loadScopedDevices(home: String) {
        let url = URL(fileURLWithPath: home, isDirectory: true)
            .appendingPathComponent("mobile")
            .appendingPathComponent("devices.json")
        guard let data = try? Data(contentsOf: url),
              let file = try? JSONDecoder().decode(ScopedPairedDeviceFile.self, from: data)
        else {
            scopedDevices = []
            return
        }
        scopedDevices = file.devices
    }

    private func scopedWorkspaceSection(_ scope: (home: String, name: String)) -> some View {
        Section {
            if scopedDevices.isEmpty {
                Text("No devices are paired with \(scope.name).")
                    .font(.system(size: 13))
                    .foregroundStyle(Theme.mutedForeground)
            } else {
                ForEach(scopedDevices) { device in
                    VStack(alignment: .leading, spacing: 2) {
                        Text(device.name)
                            .font(.system(size: 13, weight: .medium))
                            .foregroundStyle(Theme.foreground)
                            .lineLimit(1)
                        if let platform = device.platform, !platform.isEmpty {
                            Text(platform)
                                .font(.system(size: 11))
                                .foregroundStyle(Theme.mutedForeground)
                                .lineLimit(1)
                        }
                    }
                }
            }

            Button {
                store.beginScopedWorkspacePairing()
                workspaceSharePresented = true
            } label: {
                Label("Pair a Device with \(scope.name)…", systemImage: "plus")
            }
            .sheet(isPresented: $workspaceSharePresented, onDismiss: {
                store.cancelScopedWorkspacePairing()
                loadScopedDevices(home: scope.home)
            }) {
                ShareWorkspaceSheet(store: store, home: scope.home, workspaceName: scope.name)
            }
        } header: {
            SettingsSectionHeader(
                title: "Controls \(scope.name)",
                description: "Each workspace pairs its own devices — a device "
                    + "paired here reaches only \(scope.name). Revoke devices "
                    + "from \(scope.name)'s own Remote Control settings."
            )
        }
    }

    /// Link enrollment and the seat live with each instance/machine — say
    /// where, instead of silently hiding them for a scoped workspace.
    private func scopedLinkFootnote(_ scope: (home: String, name: String)) -> some View {
        Section {
            Text("Unpeel Link enrollment for \(scope.name)'s devices lives in "
                + "\(scope.name)'s own Remote Control settings; the license "
                + "covers every workspace on this Mac and is managed from "
                + "\(UnpeelWorkspaceContext.advertisedHostName)'s scope.")
                .font(.system(size: 12))
                .foregroundStyle(Theme.mutedForeground)
                .fixedSize(horizontal: false, vertical: true)
        }
    }

    /// A remote Host scope (SSH or paired): mint the invitation on the Host
    /// through the sealed LAN proxy when the capability is advertised;
    /// otherwise say where pairing happens instead of guessing.
    @ViewBuilder
    private var remoteHostSection: some View {
        let scopeName = store.selectedScopeDisplayName ?? "this Host"
        Section {
            if store.remoteHostRuntime.supportsHostOperation(
                RemoteHostRuntime.HostOperation.pairingInvitation
            ) {
                Button {
                    remoteHostSharePresented = true
                } label: {
                    Label("Pair a Device with \(scopeName)…", systemImage: "plus")
                }
                .sheet(isPresented: $remoteHostSharePresented) {
                    RemoteHostPairingSheet(store: store, hostName: scopeName)
                }
            } else {
                Text("\(scopeName) cannot mint pairing invitations over this "
                    + "connection. Pair devices from its own running Unpeel "
                    + "instead — the terminal UI's Settings ▸ Remote, or "
                    + "`unpeel pair`.")
                    .font(.system(size: 13))
                    .foregroundStyle(Theme.mutedForeground)
                    .fixedSize(horizontal: false, vertical: true)
            }
        } header: {
            SettingsSectionHeader(
                title: "Controls \(scopeName)",
                description: "Each workspace pairs its own devices — a device "
                    + "paired here reaches only \(scopeName), and its entry is "
                    + "revocable on \(scopeName) itself."
            )
        }
    }

    /// Banner pointing users at the iPhone app's TestFlight beta. Sits
    /// directly below the inbound list because installing the phone app is
    /// step zero of pairing it.
    private var testflightSection: some View {
        Section {
            HStack(alignment: .center, spacing: 16) {
                TestFlightIconView()
                    .frame(width: 84, height: 84)

                VStack(alignment: .leading, spacing: 6) {
                    Text("Unpeel for iPhone is in beta")
                        .font(.system(size: 13, weight: .semibold))
                        .foregroundStyle(Theme.foreground)
                    Text(
                        hasMultipleLocalWorkspaces
                            ? "Join the TestFlight beta to control this workspace from your phone. Open the invite link on your iPhone to install it with TestFlight."
                            : "Join the TestFlight beta to control this Mac from your phone. Open the invite link on your iPhone to install it with TestFlight."
                    )
                        .font(.system(size: 12))
                        .foregroundStyle(Theme.mutedForeground)
                        .fixedSize(horizontal: false, vertical: true)

                    Button("Join the Beta") {
                        NSWorkspace.shared.open(Self.iosAppURL)
                    }
                    .controlSize(.small)
                }
                .frame(maxWidth: .infinity, alignment: .leading)
            }
            .padding(.vertical, 6)
            .listRowBackground(
                RoundedRectangle(cornerRadius: 8, style: .continuous)
                    .fill(Theme.accent.opacity(0.09))
            )
        }
    }

    /// The inbound list — Controllers that drive this workspace. Mirrors the
    /// outbound "Remote" workspaces list on the Workspaces panel: rows are
    /// paired devices, and the add verb mints and shows a one-time code
    /// ("Share This Workspace…" once more than one local workspace exists;
    /// "Share This Mac…" on a one-workspace install).
    private var controlsThisMacSection: some View {
        Section {
            if let error = management.value.error {
                Text(error)
                    .font(.system(size: 13))
                    .foregroundStyle(.red)
                    .fixedSize(horizontal: false, vertical: true)
            }

            SettingsValueRow(
                label: "Local access",
                value: management.value.endpoint == nil ? "Unavailable" : "Serving on this network"
            )

            if management.value.devices.isEmpty {
                Text("No paired devices.")
                    .font(.system(size: 13))
                    .foregroundStyle(Theme.mutedForeground)
            } else {
                ForEach(management.value.devices) { device in
                    LabeledContent {
                        HStack(spacing: 12) {
                            // Link scope is managed in the Unpeel Link
                            // section below; this is just a reminder of the
                            // device's current reach.
                            Text(device.relayAllowed != false ? "Link" : "Direct only")
                                .font(.system(size: 11))
                                .foregroundStyle(Theme.mutedForeground)

                            Button("Revoke", role: .destructive) {
                                store.revokeMobileDevice(device.id)
                            }
                            .buttonStyle(.borderless)
                            .controlSize(.small)
                        }
                    } label: {
                        VStack(alignment: .leading, spacing: 2) {
                            Text(device.name)
                                .font(.system(size: 13, weight: .medium))
                                .foregroundStyle(Theme.foreground)
                                .lineLimit(1)
                            Text(deviceDetail(device))
                                .font(.system(size: 11))
                                .foregroundStyle(Theme.mutedForeground)
                                .lineLimit(1)
                        }
                    }
                }
            }

            Button {
                sharePresented = true
            } label: {
                Label(
                    hasMultipleLocalWorkspaces
                        ? "Share This Workspace…"
                        : "Share This Mac…",
                    systemImage: "plus"
                )
            }
            .sheet(isPresented: $sharePresented) {
                ShareThisMacSheet(
                    store: store,
                    usesWorkspaceLanguage: hasMultipleLocalWorkspaces
                )
            }

            if hasMultipleLocalWorkspaces {
                Text("Each workspace on this Mac is shared separately.")
                    .font(.system(size: 11))
                    .foregroundStyle(Theme.mutedForeground)
                    .fixedSize(horizontal: false, vertical: true)
            }
        } header: {
            SettingsSectionHeader(
                title: hasMultipleLocalWorkspaces
                    ? "Controls This Workspace"
                    : "Controls This Mac",
                description: "Devices pair directly over your network and receive their own revocable credential. Revoking one immediately invalidates it."
            )
        }
    }

    private var securitySection: some View {
        Section {
            SettingsValueRow(label: "Authentication", value: "Per-device bearer token")
            SettingsValueRow(label: "Pairing code", value: "One-time, 5 minutes")
            SettingsValueRow(label: "Stored token", value: "SHA-256 hash")
        } header: {
            SettingsSectionHeader(
                title: "Security",
                description: "The hook and MCP servers stay localhost-only. Remote Controllers use a separate LAN server."
            )
        }
    }

}

/// "iOS 1.2 • last seen Aug 13, 09:41" — shared by the Controls This Mac
/// list and the Unpeel Link enrollment list so both describe a device the
/// same way.
private func deviceDetail(_ device: RemotePairedDeviceSummary) -> String {
    let lastSeen: String
    if let lastSeenAt = device.lastSeenAtUnixMs {
        let date = Date(timeIntervalSince1970: TimeInterval(lastSeenAt) / 1000)
        lastSeen = "last seen \(date.formatted(date: .abbreviated, time: .shortened))"
    } else {
        lastSeen = "never seen"
    }
    let version = device.appVersion.map { " \($0)" } ?? ""
    return "\(device.platform)\(version) • \(lastSeen)"
}

/// Share-this-workspace sheet — mints a one-time pairing code on open and
/// shows the QR until a Controller consumes it. Pairing is always
/// workspace-scoped (one pairing = one workspace). The title keeps the
/// friendlier "Share This Mac" copy on a one-workspace install, and names
/// the workspace boundary once more than one local workspace exists.
/// Minting on open (rather than on every Remote-panel visit) keeps a live
/// code off the screen until the user actually intends to pair; re-minting
/// is free — it just replaces the single active one-time token. Dismissing
/// keeps the code valid until its TTL so copy-then-paste-on-another-Mac
/// flows survive closing the sheet.
struct ShareThisMacSheet: View {
    @ObservedObject var store: UnpeelStore
    @ObservedObject private var management: HostManagementState
    var usesWorkspaceLanguage: Bool = false
    @Environment(\.dismiss) private var dismiss
    @State private var now = Date()
    @State private var cliCommandCopied = false

    init(store: UnpeelStore, usesWorkspaceLanguage: Bool = false) {
        self.store = store
        self.management = store.hostManagement
        self.usesWorkspaceLanguage = usesWorkspaceLanguage
    }

    private var shareTitle: String {
        usesWorkspaceLanguage ? "Share This Workspace" : "Share This Mac"
    }

    private var shareSubtitle: String {
        usesWorkspaceLanguage
            ? "Let another Unpeel device control this workspace."
            : "Let another Unpeel device control this Mac."
    }

    /// This Mac's Bonjour/DNS name, ready to paste into an SSH target from
    /// another machine on the same network.
    private static let sshHostName = ProcessInfo.processInfo.hostName

    /// The terminal counterpart of the QR code: the `unpeel` CLI controls
    /// this Mac over the operator's existing SSH access (the Host-side
    /// stdio gateway). SSH is a transport for the same Host contract — no
    /// pairing code involved, so it sits alongside the QR, not inside it.
    private var cliCommand: String {
        "unpeel --host ssh://\(Self.sshHostName)"
    }

    private var expiry: Date? {
        guard let payload = store.hostPairingPayload else { return nil }
        return Date(timeIntervalSince1970: TimeInterval(payload.expiresAtUnixMs) / 1000)
    }

    private var expiresInText: String {
        guard let expiry else { return "" }
        let remaining = max(0, Int(expiry.timeIntervalSince(now).rounded(.down)))
        return String(format: "Expires in %d:%02d", remaining / 60, remaining % 60)
    }

    var body: some View {
        VStack(alignment: .leading, spacing: 18) {
            HStack {
                VStack(alignment: .leading, spacing: 4) {
                    Text(shareTitle)
                        .font(.system(size: 20, weight: .semibold))
                    Text(shareSubtitle)
                        .font(.system(size: 12))
                        .foregroundStyle(Theme.mutedForeground)
                }
                Spacer()
                Button("Done") { dismiss() }
                    .keyboardShortcut(.cancelAction)
            }

            if let error = management.value.error {
                Text(error)
                    .font(.system(size: 13))
                    .foregroundStyle(.red)
                    .fixedSize(horizontal: false, vertical: true)
            }

            if store.hostPairingCompleted {
                HStack(spacing: 14) {
                    Image(systemName: "checkmark.circle.fill")
                        .font(.system(size: 34))
                        .foregroundStyle(.green)
                    VStack(alignment: .leading, spacing: 5) {
                        Text("Controller paired")
                            .font(.system(size: 13, weight: .semibold))
                        Text("The displayed one-time code has been consumed.")
                            .font(.system(size: 12))
                            .foregroundStyle(Theme.mutedForeground)
                        Button("Pair Another Controller") {
                            store.beginHostPairing()
                        }
                        .controlSize(.small)
                    }
                    Spacer()
                }
                .padding(.vertical, 12)
            } else {
                HStack(alignment: .top, spacing: 18) {
                    PairingQRCodeView(payload: store.hostPairingCode)
                        .frame(width: 184, height: 184)

                    VStack(alignment: .leading, spacing: 10) {
                        Text("Scan or paste this code into another Unpeel device.")
                            .font(.system(size: 13, weight: .semibold))
                            .foregroundStyle(Theme.foreground)
                        Text("The code expires in five minutes and can be used once. After pairing, that Controller receives its own revocable device token.")
                            .font(.system(size: 12))
                            .foregroundStyle(Theme.mutedForeground)
                            .fixedSize(horizontal: false, vertical: true)

                        if store.hostPairingPayload != nil {
                            Text(expiresInText)
                                .font(.system(size: 12, weight: .medium))
                                .monospacedDigit()
                                .foregroundStyle(Theme.mutedForeground)
                        }

                        HStack(spacing: 8) {
                            Button(store.hostPairingPayload == nil ? "Generate QR Code" : "Refresh QR Code") {
                                store.beginHostPairing()
                            }
                            .controlSize(.small)

                            if store.hostPairingPayload != nil {
                                Button("Copy Pairing Code") {
                                    guard let code = store.hostPairingCode else { return }
                                    NSPasteboard.general.clearContents()
                                    NSPasteboard.general.setString(code, forType: .string)
                                }
                                .controlSize(.small)
                            }
                        }
                    }
                    .frame(maxWidth: .infinity, alignment: .leading)
                }
            }

            Divider()

            // The CLI route: same Host, driven over SSH from any terminal.
            VStack(alignment: .leading, spacing: 6) {
                Text("Or connect from a terminal")
                    .font(.system(size: 13, weight: .semibold))
                    .foregroundStyle(Theme.foreground)

                HStack(spacing: 8) {
                    Text(cliCommand)
                        .font(.system(size: 12, design: .monospaced))
                        .textSelection(.enabled)
                        .lineLimit(1)
                        .truncationMode(.middle)
                        .padding(.horizontal, 8)
                        .padding(.vertical, 5)
                        .background(
                            RoundedRectangle(cornerRadius: 6, style: .continuous)
                                .fill(Theme.foreground.opacity(0.06))
                        )

                    Button(cliCommandCopied ? "Copied" : "Copy") {
                        NSPasteboard.general.clearContents()
                        NSPasteboard.general.setString(cliCommand, forType: .string)
                        cliCommandCopied = true
                    }
                    .controlSize(.small)
                }

                Text("Run this on another machine with the Unpeel CLI installed "
                    + "(curl -fsSL https://unpeel.com/install.sh | sh). It rides your "
                    + "normal SSH access instead of a pairing code, so this Mac needs "
                    + "Remote Login on (System Settings ▸ General ▸ Sharing) and your "
                    + "SSH config must reach it — over a VPN or Tailscale too, but "
                    + "never through Unpeel Link.")
                    .font(.system(size: 11))
                    .foregroundStyle(Theme.mutedForeground)
                    .fixedSize(horizontal: false, vertical: true)
            }
        }
        .padding(24)
        .frame(width: 520)
        .onAppear { store.beginHostPairing() }
        // Live countdown, and never leave an expired QR on screen — an
        // expired code silently stops scanning, so re-mint at zero.
        .onReceive(Timer.publish(every: 1, on: .main, in: .common).autoconnect()) { tick in
            now = tick
            if let expiry, expiry <= tick, !store.hostPairingCompleted {
                store.beginHostPairing()
            }
        }
    }
}

/// Pair a device with a SELECTED local workspace, from this window (one
/// pairing = one workspace). The workspace's own instance mints the code
/// over the loopback bridge — launched first when it isn't running — and
/// completes the pairing itself; this sheet only displays the QR and
/// watches the workspace's device list to confirm the pairing landed.
struct ShareWorkspaceSheet: View {
    @ObservedObject var store: UnpeelStore
    let home: String
    let workspaceName: String
    @Environment(\.dismiss) private var dismiss
    @State private var now = Date()
    /// Device ids present when the sheet opened; a new id appearing in the
    /// workspace's `devices.json` is the pairing-completed signal (the
    /// exchange itself happens in the sibling instance).
    @State private var baselineDeviceIDs: Set<String>?
    @State private var pairedDeviceName: String?

    private var expiry: Date? {
        guard let payload = store.scopedWorkspacePairingPayload else { return nil }
        return Date(timeIntervalSince1970: TimeInterval(payload.expiresAtUnixMs) / 1000)
    }

    private var expiresInText: String {
        guard let expiry else { return "" }
        let remaining = max(0, Int(expiry.timeIntervalSince(now).rounded(.down)))
        return String(format: "Expires in %d:%02d", remaining / 60, remaining % 60)
    }

    var body: some View {
        VStack(alignment: .leading, spacing: 18) {
            HStack {
                VStack(alignment: .leading, spacing: 4) {
                    Text("Pair a Device with \(workspaceName)")
                        .font(.system(size: 20, weight: .semibold))
                    Text("The device pairs with \(workspaceName) only — each workspace pairs its own devices.")
                        .font(.system(size: 12))
                        .foregroundStyle(Theme.mutedForeground)
                }
                Spacer()
                Button("Done") { dismiss() }
                    .keyboardShortcut(.cancelAction)
            }

            if let error = store.scopedWorkspacePairingError {
                VStack(alignment: .leading, spacing: 8) {
                    Text(error)
                        .font(.system(size: 13))
                        .foregroundStyle(.red)
                        .fixedSize(horizontal: false, vertical: true)
                    Button("Try Again") {
                        store.beginScopedWorkspacePairing()
                    }
                    .controlSize(.small)
                }
            } else if let pairedDeviceName {
                HStack(spacing: 14) {
                    Image(systemName: "checkmark.circle.fill")
                        .font(.system(size: 34))
                        .foregroundStyle(.green)
                    VStack(alignment: .leading, spacing: 5) {
                        Text("\(pairedDeviceName) paired with \(workspaceName)")
                            .font(.system(size: 13, weight: .semibold))
                        Text("The displayed one-time code has been consumed.")
                            .font(.system(size: 12))
                            .foregroundStyle(Theme.mutedForeground)
                        Button("Pair Another Device") {
                            self.pairedDeviceName = nil
                            baselineDeviceIDs = currentDeviceIDs()
                            store.beginScopedWorkspacePairing()
                        }
                        .controlSize(.small)
                    }
                    Spacer()
                }
                .padding(.vertical, 12)
            } else if store.scopedWorkspacePairingPayload == nil {
                HStack(spacing: 12) {
                    ProgressView()
                        .controlSize(.small)
                    Text("Waiting for \(workspaceName)… Its instance starts in the background if it isn't running.")
                        .font(.system(size: 13))
                        .foregroundStyle(Theme.mutedForeground)
                        .fixedSize(horizontal: false, vertical: true)
                }
                .padding(.vertical, 20)
            } else {
                HStack(alignment: .top, spacing: 18) {
                    PairingQRCodeView(payload: store.scopedWorkspacePairingCode)
                        .frame(width: 184, height: 184)

                    VStack(alignment: .leading, spacing: 10) {
                        Text("Scan or paste this code into another Unpeel device.")
                            .font(.system(size: 13, weight: .semibold))
                            .foregroundStyle(Theme.foreground)
                        Text("The code expires in five minutes and can be used once. The device then holds its own revocable entry in \(workspaceName)'s device list.")
                            .font(.system(size: 12))
                            .foregroundStyle(Theme.mutedForeground)
                            .fixedSize(horizontal: false, vertical: true)

                        Text(expiresInText)
                            .font(.system(size: 12, weight: .medium))
                            .monospacedDigit()
                            .foregroundStyle(Theme.mutedForeground)

                        HStack(spacing: 8) {
                            Button("Refresh QR Code") {
                                store.beginScopedWorkspacePairing()
                            }
                            .controlSize(.small)

                            Button("Copy Pairing Code") {
                                guard let code = store.scopedWorkspacePairingCode else { return }
                                NSPasteboard.general.clearContents()
                                NSPasteboard.general.setString(code, forType: .string)
                            }
                            .controlSize(.small)
                        }
                    }
                    .frame(maxWidth: .infinity, alignment: .leading)
                }
            }
        }
        .padding(24)
        .frame(width: 520)
        .onAppear {
            baselineDeviceIDs = currentDeviceIDs()
        }
        // Countdown + re-mint at expiry (an expired code silently stops
        // scanning), and watch the workspace's device list for the pairing
        // to land — the exchange completes in the sibling instance.
        .onReceive(Timer.publish(every: 1, on: .main, in: .common).autoconnect()) { tick in
            now = tick
            if pairedDeviceName == nil, let baseline = baselineDeviceIDs,
               let newDevice = loadDevices().first(where: { !baseline.contains($0.id) }) {
                pairedDeviceName = newDevice.name
                return
            }
            if let expiry, expiry <= tick, pairedDeviceName == nil {
                store.beginScopedWorkspacePairing()
            }
        }
    }

    private func loadDevices() -> [RemoteSettingsPanel.ScopedPairedDevice] {
        let url = URL(fileURLWithPath: home, isDirectory: true)
            .appendingPathComponent("mobile")
            .appendingPathComponent("devices.json")
        struct File: Decodable { let devices: [RemoteSettingsPanel.ScopedPairedDevice] }
        guard let data = try? Data(contentsOf: url),
              let file = try? JSONDecoder().decode(File.self, from: data)
        else { return [] }
        return file.devices
    }

    private func currentDeviceIDs() -> Set<String> {
        Set(loadDevices().map(\.id))
    }
}

/// Settings ▸ Remote ▸ On Unpeel Link — the enrollment list that replaced
/// the global relay toggle (2026-08-13). Devices listed here ride the
/// encrypted relay away from home; everything else stays Direct-only. Two
/// kinds of rows, one list: inbound paired Controllers (the per-device
/// `relayAllowed` flag) and outbound paired Hosts (the per-Host
/// `linkEnabled` flag). The uplink runs whenever ≥1 inbound device is
/// enrolled — the entitlement check itself stays server-side
/// (the Link service contract).
private struct LinkEnrollmentSection: View {
    @ObservedObject var store: UnpeelStore
    @ObservedObject private var management: HostManagementState
    @ObservedObject var hosts: RemoteHostStore
    let usesWorkspaceLanguage: Bool
    @ObservedObject private var uplink = RelayUplinkManager.shared

    init(store: UnpeelStore, hosts: RemoteHostStore, usesWorkspaceLanguage: Bool) {
        self.store = store
        self.hosts = hosts
        self.management = store.hostManagement
        self.usesWorkspaceLanguage = usesWorkspaceLanguage
    }

    private var reachDescription: String {
        let target = usesWorkspaceLanguage ? "this workspace" : "this Mac"
        return "These devices reach \(target) — and these Hosts stay "
            + "reachable — from any network, through the unpeel.com "
            + "relay. Session traffic is end-to-end encrypted; notification "
            + "titles pass through Unpeel and Apple Push. Everything not listed "
            + "here connects direct-only, on your own network."
    }

    private var enrolledDevices: [RemotePairedDeviceSummary] {
        management.value.devices.filter { $0.relayAllowed != false }
    }

    private var directOnlyDevices: [RemotePairedDeviceSummary] {
        management.value.devices.filter { $0.relayAllowed == false }
    }

    // Outbound Hosts appear only where the Host picker exists at all.
    private var enrolledHosts: [PairedHostRecord] {
        RemoteHostFeature.pickerEnabled ? hosts.records.filter(\.isLinkEnabled) : []
    }

    private var directOnlyHosts: [PairedHostRecord] {
        RemoteHostFeature.pickerEnabled ? hosts.records.filter { !$0.isLinkEnabled } : []
    }

    private var addCandidatesEmpty: Bool {
        directOnlyDevices.isEmpty && directOnlyHosts.isEmpty
    }

    var body: some View {
        Section {
            if enrolledDevices.isEmpty, enrolledHosts.isEmpty {
                Text("Nothing is on Link — every connection stays direct, on your own network.")
                    .font(.system(size: 13))
                    .foregroundStyle(Theme.mutedForeground)
            }

            ForEach(enrolledDevices) { device in
                enrollmentRow(
                    icon: "iphone",
                    name: device.name,
                    detail: deviceDetail(device)
                ) {
                    store.setDeviceRelayAllowed(device.id, false)
                }
            }

            ForEach(enrolledHosts) { host in
                enrollmentRow(
                    icon: "server.rack",
                    name: host.name,
                    detail: host.hostID
                ) {
                    store.setHostLinkEnabled(host.hostID, false)
                }
            }

            // The uplink serves inbound devices; without one enrolled there
            // is nothing to report.
            if !enrolledDevices.isEmpty {
                SettingsValueRow(label: "Relay", value: uplink.status.label)
            }

            Menu {
                ForEach(directOnlyDevices) { device in
                    Button {
                        store.setDeviceRelayAllowed(device.id, true)
                    } label: {
                        Label(device.name, systemImage: "iphone")
                    }
                }
                ForEach(directOnlyHosts) { host in
                    Button {
                        store.setHostLinkEnabled(host.hostID, true)
                    } label: {
                        Label(host.name, systemImage: "server.rack")
                    }
                }
            } label: {
                Label("Add to Link…", systemImage: "plus")
            }
            .disabled(addCandidatesEmpty)
            .help(addCandidatesEmpty
                ? "Everything paired is already on Link. Pair a new device or Host from the lists above first."
                : "Enroll a paired device or Host on Unpeel Link.")
        } header: {
            SettingsSectionHeader(
                title: "Reachable outside your network (Unpeel Link)",
                description: reachDescription
            )
        }
    }

    private func enrollmentRow(
        icon: String,
        name: String,
        detail: String,
        remove: @escaping () -> Void
    ) -> some View {
        LabeledContent {
            // A real bordered button: rendered borderless this read as a
            // status caption ("Direct Only") instead of the remove action,
            // which made enrollment look un-toggleable (2026-08-13).
            Button("Remove from Link", action: remove)
                .buttonStyle(.bordered)
                .controlSize(.small)
                .help("Take this device off Unpeel Link — it then connects only over your own network. Re-add it any time with \u{201C}Add to Link…\u{201D}.")
        } label: {
            HStack(spacing: 10) {
                Image(systemName: icon)
                    .font(.system(size: 13))
                    .foregroundStyle(Theme.mutedForeground)
                    .frame(width: 18)
                VStack(alignment: .leading, spacing: 2) {
                    Text(name)
                        .font(.system(size: 13, weight: .medium))
                        .foregroundStyle(Theme.foreground)
                        .lineLimit(1)
                    Text(detail)
                        .font(.system(size: 11))
                        .foregroundStyle(Theme.mutedForeground)
                        .lineLimit(1)
                }
            }
        }
    }
}

private struct TestFlightIconView: View {
    private static let image: NSImage? = {
        guard let url = ModuleResources.url(forResource: "TestFlightIcon", withExtension: "png") else {
            return nil
        }
        return NSImage(contentsOf: url)
    }()

    var body: some View {
        ZStack {
            RoundedRectangle(cornerRadius: 20, style: .continuous)
                .fill(Color.black.opacity(0.12))

            if let image = Self.image {
                Image(nsImage: image)
                    .resizable()
                    .interpolation(.high)
                    .aspectRatio(contentMode: .fit)
            } else {
                Image(systemName: "airplane")
                    .font(.system(size: 34, weight: .semibold))
                    .foregroundStyle(Theme.accent)
            }
        }
        .clipShape(RoundedRectangle(cornerRadius: 20, style: .continuous))
        .overlay(
            RoundedRectangle(cornerRadius: 20, style: .continuous)
                .strokeBorder(Color.white.opacity(0.16))
        )
        .shadow(color: Color.black.opacity(0.18), radius: 8, y: 3)
        .accessibilityLabel("TestFlight")
    }
}

struct PairingQRCodeView: View {
    let payload: String?

    var body: some View {
        ZStack {
            RoundedRectangle(cornerRadius: 10, style: .continuous)
                .fill(Color.white)
            if let image = qrImage {
                Image(nsImage: image)
                    .interpolation(.none)
                    .resizable()
                    .aspectRatio(contentMode: .fit)
                    .padding(12)
            } else {
                VStack(spacing: 6) {
                    Image(systemName: "qrcode")
                        .font(.system(size: 34, weight: .regular))
                    Text("No Code")
                        .font(.system(size: 12, weight: .medium))
                }
                .foregroundStyle(Color.black.opacity(0.38))
            }
        }
        .overlay(
            RoundedRectangle(cornerRadius: 10, style: .continuous)
                .strokeBorder(Theme.foreground.opacity(0.08))
        )
    }

    private var qrImage: NSImage? {
        guard let payload, let data = payload.data(using: .utf8) else { return nil }
        guard let filter = CIFilter(name: "CIQRCodeGenerator") else { return nil }
        filter.setValue(data, forKey: "inputMessage")
        // Lowest correction level: combined with the compact pairing code
        // this yields the coarsest (fastest-scanning) grid. The code sits on
        // a screen, not a scuffed sticker — damage tolerance buys nothing.
        filter.setValue("L", forKey: "inputCorrectionLevel")
        guard let output = filter.outputImage else { return nil }
        let image = output.transformed(by: CGAffineTransform(scaleX: 10, y: 10))
        let rep = NSCIImageRep(ciImage: image)
        let nsImage = NSImage(size: rep.size)
        nsImage.addRepresentation(rep)
        return nsImage
    }
}

// MARK: - Unpeel Sessions MCP panel


/// How Unpeel gets the user's attention: the menu-waiting attention badge and
/// the macOS/phone notification banners. General app behavior — nothing here
/// is tied to the Sessions MCP.
struct NotificationsSettingsPanel: View {
    @ObservedObject var store: UnpeelStore
    @ObservedObject private var uplink = RelayUplinkManager.shared
    @ObservedObject private var notifier = DesktopNotifier.shared

    var body: some View {
        VStack(alignment: .leading, spacing: 0) {
            Form {
                Section {} header: {
                    SettingsPaneHeader(
                        title: "Notifications",
                        description: "How Unpeel flags a session that needs you and "
                            + "when it sends a notification banner."
                    )
                    .padding(.bottom, 4)
                }

                // Decision 4 generalized: a workspace instance inherits the
                // default workspace's notification settings until it sets
                // its own; offer the revert right here.
                if !UnpeelWorkspaceContext.isDefaultInstance {
                    Section {
                        Button(
                            "Use \(UnpeelWorkspaceContext.defaultWorkspaceName ?? "Personal")'s notifications"
                        ) {
                            store.revertNotificationsToInheritedBaseline()
                        }
                        .disabled(!UnpeelStore.hasOwnMenuAttentionSetting)
                    } header: {
                        SettingsSectionHeader(
                            title: "Inherits from \(UnpeelWorkspaceContext.defaultWorkspaceName ?? "Personal")",
                            description: "This workspace uses the default "
                                + "workspace's notification settings until a "
                                + "setting below is changed. Revert drops its "
                                + "own values."
                        )
                    }
                }

                menuAttentionSection
                notificationsSection
            }
            .formStyle(.grouped)
            .scrollContentBackground(.hidden)
        }
    }

    /// Toggle for surfacing the attention badge when an agent draws a select
    /// menu (Claude/Codex numbered prompts). These fire no lifecycle hook, so
    /// the host detects them from the rendered screen; some users may prefer to
    /// keep the busy spinner instead.
    private var menuAttentionSection: some View {
        Section {
            LabeledContent {
                Toggle(
                    "",
                    isOn: Binding(
                        get: { store.menuAttentionDetectionEnabled },
                        set: { store.menuAttentionDetectionEnabled = $0 }
                    )
                )
                .toggleStyle(.switch)
                .labelsHidden()
                .controlSize(.small)
            } label: {
                VStack(alignment: .leading, spacing: 1) {
                    Text("Flag menus waiting for a choice")
                        .font(.system(size: 13))
                        .foregroundStyle(Theme.foreground)
                    Text("Show the yellow attention dot when an agent draws a "
                        + "pick-an-option menu. These prompts send no signal on "
                        + "their own, so Unpeel reads them off the screen.")
                        .font(.system(size: 11))
                        .foregroundStyle(Theme.mutedForeground)
                        .fixedSize(horizontal: false, vertical: true)
                }
            }
        } header: {
            SettingsSectionHeader(
                title: "Attention",
                description: "When a session is waiting for you to answer an "
                    + "on-screen menu."
            )
        }
    }

    private var notificationsSection: some View {
        Section {
            Button(notifier.lastTestDiagnostic.isChecking
                ? "Sending…"
                : "Send a test Mac notification"
            ) {
                notifier.sendTestNotification()
            }
            .disabled(notifier.lastTestDiagnostic.isChecking)

            SettingsValueRow(
                label: "Last Mac test",
                value: notifier.lastTestDiagnostic.label
            )

            if notifier.lastTestDiagnostic.needsSystemSettings {
                Button("Open Mac Notification Settings…") {
                    if let url = URL(
                        string: "x-apple.systempreferences:com.apple.Notifications-Settings.extension"
                    ) {
                        NSWorkspace.shared.open(url)
                    }
                }
            }

            Button("Send a test phone notification") {
                store.sendTestPhoneNotification()
            }
            .disabled(store.mobilePushTargetCount == 0)

            SettingsValueRow(
                label: "Paired phone tokens",
                value: store.mobilePushTargetCount == 0
                    ? "None registered"
                    : "\(store.mobilePushTargetCount) ready"
            )
            SettingsValueRow(label: "Unpeel Link", value: uplink.status.label)
            SettingsValueRow(label: "Last phone push", value: uplink.lastPushDiagnostic.label)
            if let attemptedAt = uplink.lastPushAttemptAt {
                SettingsValueRow(
                    label: "Last attempt",
                    value: attemptedAt.formatted(date: .abbreviated, time: .standard)
                )
            }
        } header: {
            SettingsSectionHeader(
                title: "Notifications",
                description: "A macOS banner (and a push to a paired iPhone) when a session "
                    + "needs input, or finishes if you turned on \u{201C}Notify when done\u{201D} "
                    + "for it. Phone alerts use Link/APNs even while terminal traffic stays "
                    + "Direct or SSH. Mac and phone tests exercise their respective delivery "
                    + "paths; phone diagnostics distinguish a missing APNs token, Link "
                    + "entitlement failure, and APNs rejection."
            )
        }
    }

}

// MARK: - Transcripts panel

/// Which content types the Markdown transcript includes and how much of it.
/// Shared by the session context menu's "Copy transcript" action (desktop and
/// phone) and the Sessions MCP `read_transcript` tool (as its defaults), so
/// all of them stay in sync.
struct TranscriptsSettingsPanel: View {
    @ObservedObject var store: UnpeelStore

    var body: some View {
        VStack(alignment: .leading, spacing: 0) {
            Form {
                Section {} header: {
                    SettingsPaneHeader(
                        title: "Transcripts",
                        description: "A session's conversation, rendered as Markdown — "
                            + "what \"Copy transcript\" copies and what agents read."
                    )
                    .padding(.bottom, 4)
                }

                transcriptSection
            }
            .formStyle(.grouped)
            .scrollContentBackground(.hidden)
        }
    }

    private var transcriptSection: some View {
        Section {
            transcriptToggle(
                title: "Session info header",
                subtitle: "Start with the session's title, ID, CLI, and model. "
                    + "The ID lets another agent target this session with the "
                    + "Sessions MCP tools.",
                on: store.transcriptSettings.includeSessionInfo
            ) { $0.includeSessionInfo = $1 }
            transcriptToggle(
                title: "User messages",
                on: store.transcriptSettings.includeUser
            ) { $0.includeUser = $1 }
            transcriptToggle(
                title: "Assistant messages",
                on: store.transcriptSettings.includeAssistant
            ) { $0.includeAssistant = $1 }
            transcriptToggle(
                title: "Reasoning",
                subtitle: "The agent's thinking blocks.",
                on: store.transcriptSettings.includeReasoning
            ) { $0.includeReasoning = $1 }
            transcriptToggle(
                title: "Tool calls & results",
                subtitle: "Commands the agent ran and their output.",
                on: store.transcriptSettings.includeTools
            ) { $0.includeTools = $1 }
            transcriptToggle(
                title: "File changes & diffs",
                on: store.transcriptSettings.includeFileChanges
            ) { $0.includeFileChanges = $1 }
            transcriptToggle(
                title: "Plan updates",
                on: store.transcriptSettings.includePlanUpdates
            ) { $0.includePlanUpdates = $1 }

            LabeledContent {
                Picker(
                    "",
                    selection: Binding(
                        get: { store.transcriptSettings.maxEntries },
                        set: { value in
                            store.updateTranscriptSettings { $0.maxEntries = value }
                        }
                    )
                ) {
                    Text("Whole conversation").tag(0)
                    Text("Last 20 entries").tag(20)
                    Text("Last 50 entries").tag(50)
                    Text("Last 100 entries").tag(100)
                }
                .labelsHidden()
                .pickerStyle(.menu)
                .fixedSize()
            } label: {
                VStack(alignment: .leading, spacing: 1) {
                    Text("Range")
                        .font(.system(size: 13))
                        .foregroundStyle(Theme.foreground)
                    Text("How much of the conversation to include.")
                        .font(.system(size: 11))
                        .foregroundStyle(Theme.mutedForeground)
                        .fixedSize(horizontal: false, vertical: true)
                }
            }
        } header: {
            SettingsSectionHeader(
                title: "Transcript content",
                description: "What \"Copy transcript\" (right-click a session) puts on "
                    + "the clipboard as Markdown. These options also drive the defaults "
                    + "for agents reading a session's transcript. Range is the default "
                    + "for agent reads; the Copy transcript menu picks its own range."
            )
        }
    }

    /// One transcript content toggle wired into the shared transcript settings.
    private func transcriptToggle(
        title: String,
        subtitle: String? = nil,
        on: Bool,
        set: @escaping (inout TranscriptSettings, Bool) -> Void
    ) -> some View {
        LabeledContent {
            Toggle(
                "",
                isOn: Binding(
                    get: { on },
                    set: { value in
                        store.updateTranscriptSettings { set(&$0, value) }
                    }
                )
            )
            .toggleStyle(.switch)
            .labelsHidden()
            .controlSize(.small)
        } label: {
            VStack(alignment: .leading, spacing: 1) {
                Text(title)
                    .font(.system(size: 13))
                    .foregroundStyle(Theme.foreground)
                if let subtitle {
                    Text(subtitle)
                        .font(.system(size: 11))
                        .foregroundStyle(Theme.mutedForeground)
                        .fixedSize(horizontal: false, vertical: true)
                }
            }
        }
    }

}

// MARK: - Browser MCP panel (Browser Access)


/// min-height 28, padding 2px 12px, radius 9, title 13px/600;
/// muted → fg + fg-10% bg on hover; active = active-tint bg + fg.
/// Muted uppercase caption that labels the "Unpeel MCP" nav group,
/// aligned with the nav rows' 12pt leading inset.
private struct SettingsNavSectionHeader: View {
    let title: String

    var body: some View {
        Text(title)
            .font(.system(size: 11, weight: .semibold))
            .foregroundStyle(Theme.mutedForeground.opacity(0.6))
            .lineLimit(1)
            .padding(EdgeInsets(top: 2, leading: 12, bottom: 2, trailing: 12))
            .frame(maxWidth: .infinity, alignment: .leading)
    }
}

private struct SettingsNavRow: View {
    let title: String
    var leadingIcon: ChromeIcon?
    let isActive: Bool
    let action: () -> Void

    @State private var hovering = false

    var body: some View {
        Button(action: action) {
            HStack(spacing: 0) {
                if let leadingIcon {
                    // .settings-leading: 18×18 slot, margin-right 6.
                    ChromeIconView(icon: leadingIcon, size: 16)
                        .frame(width: 18, height: 18)
                        .padding(.trailing, 6)
                }
                Text(title)
                    .font(.system(size: 13, weight: .semibold))
                    .lineLimit(1)
                    .truncationMode(.tail)
                Spacer(minLength: 0)
            }
            .foregroundStyle(hovering || isActive ? Theme.foreground : Theme.mutedForeground)
            .padding(EdgeInsets(top: 2, leading: 12, bottom: 2, trailing: 12))
            .frame(maxWidth: .infinity, minHeight: 28, alignment: .leading)
            .background(
                RoundedRectangle(cornerRadius: 9, style: .continuous)
                    .fill(isActive ? Theme.activeRow : (hovering ? Theme.hoverRow : .clear))
            )
            .contentShape(Rectangle())
        }
        .buttonStyle(.plain)
        .onHover { hovering = $0 }
        .animation(.easeInOut(duration: 0.12), value: hovering)
    }
}

// MARK: - System Settings grouped-form helpers
//
// Settings panels are grouped SwiftUI Forms (macOS 26 renders the System
// Settings inset-card anatomy natively: rounded section cards, hairline
// row separators inset to the labels, standard switches). The helpers
// below cover the pieces Form does not give us for free on a custom dark
// vibrancy background.

/// Large bold pane title + muted description (System Settings pane header
/// treatment). Used as the `header:` of a leading empty Section so it scrolls
/// with the content under the sticky breadcrumb — grouped-Form *rows* always
/// grow a card (`listRowBackground(.clear)` is ignored), but Section headers
/// are background-less, so the title/description live there instead.
struct SettingsPaneHeader: View {
    let title: String
    var description = ""

    var body: some View {
        VStack(alignment: .leading, spacing: 6) {
            Text(title)
                .font(.system(size: 22, weight: .bold))
                .foregroundStyle(Theme.foreground)
            if !description.isEmpty {
                Text(description)
                    .font(.system(size: 13))
                    .foregroundStyle(Theme.mutedForeground)
                    .lineSpacing(2.5)
                    .fixedSize(horizontal: false, vertical: true)
                    // Cap the measure: full-column description lines are hard
                    // to read at 700pt.
                    .frame(maxWidth: 560, alignment: .leading)
            }
        }
        .padding(.top, 4)
        .padding(.bottom, 8)
        .frame(maxWidth: .infinity, alignment: .leading)
    }
}

/// Section header block: 13pt semibold title + muted multi-line 12pt
/// description, like the "Screen & System Audio Recording" header copy in
/// System Settings. Use as a Section's `header:`.
struct SettingsSectionHeader: View {
    let title: String
    var description = ""

    var body: some View {
        VStack(alignment: .leading, spacing: 4) {
            Text(title)
                .font(.system(size: 13, weight: .semibold))
                .foregroundStyle(Theme.foreground)
            if !description.isEmpty {
                Text(description)
                    .font(.system(size: 12))
                    .foregroundStyle(Theme.mutedForeground)
                    .lineSpacing(2.5)
                    .fixedSize(horizontal: false, vertical: true)
                    // Same readable-measure cap as SettingsPaneHeader.
                    .frame(maxWidth: 560, alignment: .leading)
            }
        }
        .textCase(nil)
        // Grouped Form's own section gap is tight on the dark shell; the top
        // padding here is what separates a section from the card above it.
        .padding(.top, 14)
        .padding(.bottom, 6)
    }
}

/// Label + trailing value text row (System Settings "About"-style rows).
struct SettingsValueRow: View {
    let label: String
    let value: String

    var body: some View {
        LabeledContent {
            Text(value)
                .font(.system(size: 13))
                .monospacedDigit()
                .foregroundStyle(Theme.mutedForeground)
        } label: {
            Text(label)
                .font(.system(size: 13))
                .foregroundStyle(Theme.foreground)
        }
    }
}

// MARK: - Experimental panel

/// Data-driven list of experimental feature toggles. Every entry in
/// `ExperimentalFeature.all` renders one row here automatically, so adding a
/// future experiment needs no new UI — just a registry entry in
/// `FeatureFlags.swift`.
struct ExperimentalSettingsPanel: View {
    @ObservedObject var store: UnpeelStore

    var body: some View {
        VStack(alignment: .leading, spacing: 0) {
            Form {
                Section {} header: {
                    SettingsPaneHeader(
                        title: "Experimental",
                        description: "Early features that are still being shaped. They can "
                            + "change or disappear between releases. Turn one off here if it "
                            + "gets in the way — no restart needed."
                    )
                    .padding(.bottom, 4)
                }

                // Decision 4 generalized: a workspace instance inherits the
                // default workspace's experimental flags until it sets its
                // own; offer the revert right here.
                if !UnpeelWorkspaceContext.isDefaultInstance {
                    Section {
                        Button(
                            "Use \(UnpeelWorkspaceContext.defaultWorkspaceName ?? "Personal")'s features"
                        ) {
                            store.revertExperimentalToInheritedBaseline()
                        }
                        .disabled(
                            !ExperimentalFeature.all.contains {
                                UnpeelFeatureFlags.hasOwnSetting($0)
                            }
                        )
                    } header: {
                        SettingsSectionHeader(
                            title: "Inherits from \(UnpeelWorkspaceContext.defaultWorkspaceName ?? "Personal")",
                            description: "This workspace uses the default "
                                + "workspace's experimental features until a "
                                + "toggle below is changed. Revert drops its "
                                + "own values."
                        )
                    }
                }

                if UnpeelFeatureFlags.availableExperimentalFeatures.isEmpty {
                    Section {
                        Text("No experimental features right now. Check back after an update.")
                            .font(.system(size: 12))
                            .foregroundStyle(Theme.mutedForeground)
                    }
                } else {
                    Section {
                        ForEach(UnpeelFeatureFlags.availableExperimentalFeatures) { feature in
                            featureRow(feature)
                        }
                    }
                }
            }
            .formStyle(.grouped)
            .scrollContentBackground(.hidden)
        }
    }

    private func featureRow(_ feature: ExperimentalFeature) -> some View {
        LabeledContent {
            Toggle("", isOn: Binding(
                get: { store.isExperimentalEnabled(feature) },
                set: { store.setExperimental($0, for: feature) }
            ))
            .labelsHidden()
            .toggleStyle(.switch)
        } label: {
            VStack(alignment: .leading, spacing: 2) {
                Text(feature.title)
                    .font(.system(size: 13))
                    .foregroundStyle(Theme.foreground)
                Text(feature.summary)
                    .font(.system(size: 11))
                    .foregroundStyle(Theme.mutedForeground)
                    .lineSpacing(2)
                    .fixedSize(horizontal: false, vertical: true)
                    .frame(maxWidth: 460, alignment: .leading)
            }
        }
    }
}

// MARK: - Advanced panel (settings/AdvancedPanel.svelte)

/// Native Advanced panel: memory usage, running terminal hosts (with
/// Stop and archive),
/// cleanup policy, and diagnostics utilities.
struct AdvancedSettingsPanel: View {
    @ObservedObject var store: UnpeelStore

    @State private var memory: MemorySnapshot?
    @State private var terminals: [RunningTerminal] = []
    @State private var loading = false
    @State private var loaded = false

    var body: some View {
        VStack(alignment: .leading, spacing: 0) {
            Form {
                Section {} header: {
                    SettingsPaneHeader(
                        title: "Advanced",
                        description: "Resource usage, cleanup, and on-disk data for Unpeel's terminal hosts."
                    )
                    .padding(.bottom, 4)
                }

                cleanupSection
                memorySection
                terminalsSection
                diagnosticsSection
            }
            .formStyle(.grouped)
            .scrollContentBackground(.hidden)
        }
        .onAppear {
            if !loaded { refresh() }
        }
        .onChange(of: store.removingSessionIDs) { ids in
            // Refresh counts after a kill finishes so memory + row list stay in sync.
            if ids.isEmpty, loaded { refresh() }
        }
    }

    // MARK: Old session cleanup

    private var cleanupSection: some View {
        Section {
            LabeledContent {
                Picker(
                    "Auto-stop and archive inactive terminals",
                    selection: Binding(
                        get: { store.autoStopArchiveMinutes },
                        set: { store.setAutoStopArchiveMinutes($0) }
                    )
                ) {
                    ForEach(UnpeelStore.autoStopArchiveMinuteOptions, id: \.self) { minutes in
                        Text(UnpeelStore.autoStopArchiveLabel(for: minutes)).tag(minutes)
                    }
                }
                .labelsHidden()
                .pickerStyle(.menu)
                .frame(width: 150)
            } label: {
                Text("Auto-stop and archive inactive terminals")
                    .font(.system(size: 13))
                    .foregroundStyle(Theme.foreground)
            }
            LabeledContent {
                Picker(
                    "Stopped or archived sessions shown in sidebar",
                    selection: Binding(
                        get: { store.sidebarVisibleSessionLimit },
                        set: { store.setSidebarStoppedLimit($0) }
                    )
                ) {
                    ForEach(UnpeelStore.sidebarStoppedLimitOptions, id: \.self) { limit in
                        Text(UnpeelStore.sidebarStoppedLimitLabel(for: limit)).tag(limit)
                    }
                }
                .labelsHidden()
                .pickerStyle(.menu)
                .frame(width: 150)
            } label: {
                Text("Stopped or archived sessions shown in sidebar")
                    .font(.system(size: 13))
                    .foregroundStyle(Theme.foreground)
            }
        } header: {
            SettingsSectionHeader(
                title: "Cleanup",
                description: "Sessions that have stayed idle for the selected time are stopped and archived — the same as clicking \"Stop and archive\": the terminal stops and the session files away into the project's archive library, where Restore & Resume continues the conversation. Sessions that keep working (including loops — any activity resets the clock), or that are pinned, selected, unread, or waiting for input, are left alone; plain shell terminals are never touched. Nothing is deleted automatically. Sessions that stop or die on their own are never archived automatically. Choose how many stopped or archived sessions each project previews; older rows are hidden from the sidebar only."
            )
        }
    }

    // MARK: Memory (AdvancedPanel.svelte:214-244)

    private var memorySection: some View {
        Section {
            if let memory {
                SettingsValueRow(
                    label: "App memory (Unpeel Native)",
                    value: formatMB(memory.processFootprintBytes)
                )
                SettingsValueRow(
                    label: "Running terminal hosts",
                    value: "\(memory.runningHostCount)"
                )
                SettingsValueRow(
                    label: "Hosted sessions on disk",
                    value: "\(memory.hostedSessionCount)"
                )
            } else {
                Text(loading ? "Loading…" : "Unable to read memory usage")
                    .font(.system(size: 13))
                    .foregroundStyle(Theme.mutedForeground.opacity(0.6))
            }
        } header: {
            SettingsSectionHeader(
                title: "Memory",
                description: "Current process memory usage and active session counts."
            )
        }
    }

    // MARK: Running terminals

    private var totalCpu: Double { terminals.reduce(0) { $0 + $1.cpuPercent } }
    private var totalRss: UInt64 { terminals.reduce(0) { $0 + $1.rssBytes } }

    private var summaryText: String {
        terminals.isEmpty
            ? "Live terminal hosts sorted by current CPU usage."
            : "\(terminals.count) running · \(formatCpu(totalCpu)) CPU · \(formatMB(totalRss)) memory. Sorted by current CPU usage."
    }

    private var terminalsSection: some View {
        Section {
            if terminals.isEmpty {
                Text(loading ? "Loading terminals…" : "No running terminals.")
                    .font(.system(size: 13))
                    .foregroundStyle(Theme.mutedForeground)
            } else {
                ForEach(terminals) { terminal in
                    terminalRow(terminal)
                }
            }
        } header: {
            HStack(alignment: .top, spacing: 12) {
                SettingsSectionHeader(title: "Running Terminals", description: summaryText)
                Spacer(minLength: 8)
                Button(loading ? "Refreshing…" : "Refresh") { refresh() }
                    .controlSize(.small)
                    .disabled(loading)
                    // Track the header's built-in top padding so the button
                    // lines up with the title, not the section gap above it.
                    .padding(.top, 14)
            }
        }
    }

    /// System Settings login-items-style row: icon tile, label + sublabel,
    /// trailing CPU/Memory cells + Open / Stop and archive buttons.
    private func terminalRow(_ terminal: RunningTerminal) -> some View {
        let isRemoving = store.removingSessionIDs.contains(terminal.id)
        return HStack(alignment: .center, spacing: 12) {
            ToolIconView(tool: QuickPresetTool.detect(in: terminal.command), size: 16)
                .foregroundStyle(Theme.foreground)
                .frame(width: 28, height: 28)
                .background(
                    RoundedRectangle(cornerRadius: 7, style: .continuous)
                        .fill(Theme.foreground.opacity(0.07))
                )

            VStack(alignment: .leading, spacing: 2) {
                Text(terminal.label)
                    .font(.system(size: 13))
                    .foregroundStyle(Theme.foreground)
                    .lineLimit(1)
                    .truncationMode(.tail)
                HStack(spacing: 6) {
                    Text(terminal.commandLabel)
                        .lineLimit(1)
                        .truncationMode(.tail)
                        .frame(maxWidth: 200, alignment: .leading)
                        .fixedSize(horizontal: true, vertical: false)
                    Text("PID \(String(terminal.pid))")
                    Text("\(terminal.processCount) proc")
                    Text(compactPath(terminal.cwd))
                        .opacity(0.75)
                        .lineLimit(1)
                        .truncationMode(.middle)
                        .help(terminal.cwd)
                }
                .font(.system(size: 11))
                .foregroundStyle(Theme.mutedForeground)
            }
            .frame(maxWidth: .infinity, alignment: .leading)

            HStack(spacing: 10) {
                resourceCell(value: formatCpu(terminal.cpuPercent), label: "CPU")
                resourceCell(value: formatMB(terminal.rssBytes), label: "Memory")
            }

            HStack(spacing: 8) {
                Button("Open") {
                    store.revealSessionInSidebar(terminal.id)
                }
                .controlSize(.small)
                .disabled(isRemoving)

                if store.sessionCanArchive(terminal.id) {
                    Button("Stop and archive") {
                        store.archiveSession(terminal.id)
                        terminals.removeAll { $0.id == terminal.id }
                    }
                    .controlSize(.small)
                    .disabled(isRemoving)
                } else {
                    Button("Remove") {
                        store.requestRemoveSession(terminal.id)
                        store.revealSessionInSidebar(terminal.id)
                    }
                    .controlSize(.small)
                    .disabled(isRemoving)
                }
            }
        }
        .padding(.vertical, 2)
        .opacity(isRemoving ? 0.5 : 1)
    }

    private func resourceCell(value: String, label: String) -> some View {
        VStack(alignment: .trailing, spacing: 1) {
            Text(value)
                .font(.system(size: 12, weight: .semibold))
                .monospacedDigit()
                .foregroundStyle(Theme.foreground)
            Text(label)
                .font(.system(size: 10))
                .foregroundStyle(Theme.mutedForeground)
        }
        .frame(minWidth: 56, alignment: .trailing)
    }

    // MARK: Diagnostics (home for the old gear-menu utilities)

    private var diagnosticsSection: some View {
        Section {
            LabeledContent {
                Button("Show in Finder") {
                    NSWorkspace.shared.open(LaunchConfig.appSessionsDir)
                }
                .controlSize(.small)
            } label: {
                Text("Sessions folder")
                    .font(.system(size: 13))
                    .foregroundStyle(Theme.foreground)
            }
            LabeledContent {
                Button("Show in Finder") {
                    let trace = LaunchConfig.unpeelDir
                        .appendingPathComponent("hooks")
                        .appendingPathComponent("trace.log")
                    if FileManager.default.fileExists(atPath: trace.path) {
                        NSWorkspace.shared.activateFileViewerSelecting([trace])
                    } else {
                        NSWorkspace.shared.open(LaunchConfig.unpeelDir)
                    }
                }
                .controlSize(.small)
            } label: {
                Text("Hooks trace log")
                    .font(.system(size: 13))
                    .foregroundStyle(Theme.foreground)
            }
        } header: {
            SettingsSectionHeader(
                title: "Diagnostics",
                description: "Quick access to Unpeel's on-disk session data and hook trace log."
            )
        }
    }

    // MARK: Data collection

    private func refresh() {
        loading = true
        loaded = true
        Task.detached(priority: .userInitiated) {
            let snapshot = AdvancedDiagnostics.collect()
            await MainActor.run {
                memory = snapshot.memory
                terminals = snapshot.terminals
                loading = false
            }
        }
    }

    private func formatMB(_ bytes: UInt64) -> String {
        "\(Int((Double(bytes) / (1024 * 1024)).rounded())) MB"
    }

    private func formatCpu(_ value: Double) -> String {
        value >= 10 ? String(format: "%.0f%%", value) : String(format: "%.1f%%", value)
    }

    private func compactPath(_ path: String) -> String {
        guard !path.isEmpty else { return "No folder" }
        let parts = path.split(separator: "/").map(String.init)
        guard parts.count > 2 else { return path }
        return ".../" + parts.suffix(2).joined(separator: "/")
    }
}

// MARK: - Diagnostics data (read-only parity with get_memory_usage /
// list_running_terminals in pty_manager.rs)

struct MemorySnapshot: Sendable {
    let processFootprintBytes: UInt64
    let runningHostCount: Int
    let hostedSessionCount: Int
}

struct RunningTerminal: Identifiable, Sendable {
    let id: String // session id
    let projectID: String
    let label: String
    let command: String
    let cwd: String
    let pid: Int32
    let processCount: Int
    let cpuPercent: Double
    let rssBytes: UInt64

    var commandLabel: String {
        command.trimmingCharacters(in: .whitespaces).isEmpty ? "Blank shell" : command
    }
}

enum AdvancedDiagnostics {
    struct Snapshot: Sendable {
        let memory: MemorySnapshot
        let terminals: [RunningTerminal]
    }

    /// Manifest fields the advanced panel needs beyond what SessionEntry
    /// carries (cwd + pid live only in the manifest).
    private struct ManifestSlim: Decodable {
        struct Session: Decodable {
            let id: String
            let projectID: String
            let label: String?
            let command: String?

            enum CodingKeys: String, CodingKey {
                case id, label, command
                case projectID = "project_id"
            }
        }

        let session: Session
        let cwd: String?
        let state: String
        let pid: Int32?
    }

    static func collect() -> Snapshot {
        let fm = FileManager.default
        // Native renames win over the manifest label, same as UnpeelStore.
        let titleOverrides = (AppDefaults.shared.dictionary(
            forKey: NativeOverlay.sessionTitlesKey
        ) as? [String: String]) ?? [:]
        var hostedCount = 0
        var running: [(manifest: ManifestSlim, pid: Int32)] = []

        if let dirs = try? fm.contentsOfDirectory(
            at: LaunchConfig.appSessionsDir,
            includingPropertiesForKeys: nil,
            options: [.skipsHiddenFiles]
        ) {
            for dir in dirs {
                let url = dir.appendingPathComponent("manifest.json")
                guard let data = try? Data(contentsOf: url),
                      let manifest = try? JSONDecoder().decode(ManifestSlim.self, from: data)
                else { continue }
                hostedCount += 1
                if manifest.state == "running", let pid = manifest.pid, kill(pid, 0) == 0 {
                    running.append((manifest, pid))
                }
            }
        }

        // Whole-machine process table → per-host subtree rollup, the same
        // walk list_running_terminals does with sysinfo.
        let table = processTable()
        var children: [pid_t: [pid_t]] = [:]
        for (pid, info) in table {
            children[info.ppid, default: []].append(pid)
        }

        var terminals: [RunningTerminal] = []
        for (manifest, pid) in running {
            var processCount = 0
            var cpu = 0.0
            var rssKB: UInt64 = 0
            var stack: [pid_t] = [pid]
            var seen = Set<pid_t>()
            while let current = stack.popLast() {
                guard seen.insert(current).inserted else { continue }
                if let info = table[current] {
                    processCount += 1
                    cpu += info.cpu
                    rssKB += info.rssKB
                }
                stack.append(contentsOf: children[current] ?? [])
            }

            let command = manifest.session.command ?? ""
            let manifestLabel = (manifest.session.label?.isEmpty == false)
                ? manifest.session.label!
                : (command.isEmpty ? "Terminal" : command)
            let label = titleOverrides[manifest.session.id] ?? manifestLabel
            terminals.append(RunningTerminal(
                id: manifest.session.id,
                projectID: manifest.session.projectID,
                label: label,
                command: command,
                cwd: manifest.cwd ?? "",
                pid: pid,
                processCount: processCount,
                cpuPercent: cpu,
                rssBytes: rssKB * 1024
            ))
        }
        terminals.sort { $0.cpuPercent != $1.cpuPercent ? $0.cpuPercent > $1.cpuPercent : $0.id < $1.id }

        let memory = MemorySnapshot(
            processFootprintBytes: processFootprint(),
            runningHostCount: running.count,
            hostedSessionCount: hostedCount
        )
        return Snapshot(memory: memory, terminals: terminals)
    }

    /// `ps -axo pid=,ppid=,pcpu=,rss=` → pid-indexed cpu%/rss(KB)/ppid.
    private static func processTable() -> [pid_t: (ppid: pid_t, cpu: Double, rssKB: UInt64)] {
        let process = Process()
        process.executableURL = URL(fileURLWithPath: "/bin/ps")
        process.arguments = ["-axo", "pid=,ppid=,pcpu=,rss="]
        let pipe = Pipe()
        process.standardOutput = pipe
        process.standardError = FileHandle.nullDevice
        process.standardInput = FileHandle.nullDevice
        guard (try? process.run()) != nil else { return [:] }
        let data = pipe.fileHandleForReading.readDataToEndOfFile()
        process.waitUntilExit()

        var table: [pid_t: (ppid: pid_t, cpu: Double, rssKB: UInt64)] = [:]
        for line in String(decoding: data, as: UTF8.self).split(separator: "\n") {
            let fields = line.split(separator: " ", omittingEmptySubsequences: true)
            guard fields.count >= 4,
                  let pid = pid_t(fields[0]),
                  let ppid = pid_t(fields[1]),
                  let cpu = Double(fields[2]),
                  let rss = UInt64(fields[3])
            else { continue }
            table[pid] = (ppid, cpu, rss)
        }
        return table
    }

    /// Own-process physical footprint (the figure Activity Monitor shows),
    /// the native equivalent of get_memory_usage's process RSS.
    private static func processFootprint() -> UInt64 {
        var info = task_vm_info_data_t()
        var count = mach_msg_type_number_t(
            MemoryLayout<task_vm_info_data_t>.size / MemoryLayout<integer_t>.size
        )
        let result = withUnsafeMutablePointer(to: &info) {
            $0.withMemoryRebound(to: integer_t.self, capacity: Int(count)) {
                task_info(mach_task_self_, task_flavor_t(TASK_VM_INFO), $0, &count)
            }
        }
        guard result == KERN_SUCCESS else { return 0 }
        return UInt64(info.phys_footprint)
    }
}
