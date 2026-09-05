//
//  UnpeelMCPSettingsPanel.swift
//  UnpeelNative
//
//  Extracted from SettingsView.swift — Settings ▸ Sessions use panel.
//

import SwiftUI

/// The Sessions-domain controls inside the unified Unpeel MCP settings group.
/// The compatibility feature gate is still `sessionsMcp`; this panel explains
/// terminal-session access and lists remembered write/App-open approvals.
struct UnpeelMCPSettingsPanel: View {
    @ObservedObject var store: UnpeelStore

    var body: some View {
        VStack(alignment: .leading, spacing: 0) {
            Form {
                Section {} header: {
                    SettingsPaneHeader(
                        title: "Sessions use",
                        description: "Sessions use lets an agent session read your other "
                            + "sessions and, with your approval, type into them. Sidebar "
                            + "groups organize sessions but do not grant access."
                    )
                    .padding(.bottom, 4)
                }

                accessModelSection
                writeAccessSection
                worktreeAccessSection
                gallerySection
                approvedPairsSection
                approvedAppsSection
            }
            .formStyle(.grouped)
            .scrollContentBackground(.hidden)
        }
    }

    /// Explains the access model: reads are open everywhere and every write to
    /// another session goes through the policy below.
    private var accessModelSection: some View {
        Section {
            Text("Every session can read every other session. Writing to another session "
                + "follows the setting below — by default Unpeel asks you the first time and "
                + "remembers your answer per pair. Sidebar groups are organizational only, "
                + "and agents never create or close sessions themselves.")
                .font(.system(size: 13))
                .foregroundStyle(Theme.mutedForeground)
                .fixedSize(horizontal: false, vertical: true)
        } header: {
            SettingsSectionHeader(
                title: "Session access",
                description: "How sessions can see and control each other."
            )
        }
    }

    /// The app-wide inter-session write policy. Applied live — the host
    /// re-reads it on every write.
    /// Let sessions create Unpeel-managed worktrees (create_worktree/
    /// list_worktrees actions). Only rendered while the Worktrees
    /// experimental feature is on; session creation stays user-only either
    /// way — this grants checkout prep, not agent spawning.
    @ViewBuilder
    private var worktreeAccessSection: some View {
        if UnpeelFeatureFlags.isEnabled(.worktrees) {
            Section {
                LabeledContent {
                    Toggle(
                        "",
                        isOn: Binding(
                            get: { store.mcpWorktreeAccess },
                            set: { store.setMcpWorktreeAccess($0) }
                        )
                    )
                    .toggleStyle(.switch)
                    .labelsHidden()
                    .controlSize(.small)
                } label: {
                    VStack(alignment: .leading, spacing: 1) {
                        Text("Let sessions create worktrees")
                            .font(.system(size: 13))
                            .foregroundStyle(Theme.foreground)
                        Text("Agents can prepare isolated git worktrees as child projects "
                            + "in the sidebar. Launching sessions into them is still up to "
                            + "you. Applies immediately.")
                            .font(.system(size: 11))
                            .foregroundStyle(Theme.mutedForeground)
                            .fixedSize(horizontal: false, vertical: true)
                    }
                }
            }
        }
    }

    private var writeAccessSection: some View {
        Section {
            LabeledContent {
                Picker(
                    "",
                    selection: Binding(
                        get: { store.mcpNonChildWriteAccess },
                        set: { store.setMcpNonChildWriteAccess($0) }
                    )
                ) {
                    ForEach(McpNonChildWriteAccess.allCases) { policy in
                        Text(policy.label).tag(policy)
                    }
                }
                .labelsHidden()
                .pickerStyle(.menu)
                .fixedSize()
            } label: {
                VStack(alignment: .leading, spacing: 1) {
                    Text("Writing to other sessions")
                        .font(.system(size: 13))
                        .foregroundStyle(Theme.foreground)
                    Text(store.mcpNonChildWriteAccess.detail)
                        .font(.system(size: 11))
                        .foregroundStyle(Theme.mutedForeground)
                        .fixedSize(horizontal: false, vertical: true)
                }
            }
        } header: {
            SettingsSectionHeader(
                title: "Write access",
                description: "What happens when a session writes to another session."
            )
        }
    }

    /// Browser screenshots have always landed in the gallery. Keep that as
    /// the default while letting users who take many diagnostic captures keep
    /// them private until an agent publishes a selected image with
    /// `add_to_gallery`.
    private var gallerySection: some View {
        Section {
            LabeledContent {
                Toggle(
                    "",
                    isOn: Binding(
                        get: { store.mcpAutoAddBrowserScreenshots },
                        set: { store.setMcpAutoAddBrowserScreenshots($0) }
                    )
                )
                .toggleStyle(.switch)
                .labelsHidden()
                .controlSize(.small)
            } label: {
                VStack(alignment: .leading, spacing: 1) {
                    Text("Add browser screenshots automatically")
                        .font(.system(size: 13))
                        .foregroundStyle(Theme.foreground)
                    Text(store.mcpAutoAddBrowserScreenshots
                        ? "Browser MCP screenshots appear in the current session's gallery."
                        : "Browser captures stay out of the gallery until an agent adds a "
                            + "selected image with Sessions use. Explicit phone screenshot "
                            + "requests are still added.")
                        .font(.system(size: 11))
                        .foregroundStyle(Theme.mutedForeground)
                        .fixedSize(horizontal: false, vertical: true)
                }
            }
        } header: {
            SettingsSectionHeader(
                title: "Gallery",
                description: "Choose whether Browser MCP captures are published as they are taken."
            )
        }
    }

    /// The remembered caller→target pairs the user has approved. Revoking a
    /// pair makes the next write ask again.
    private var approvedPairsSection: some View {
        Section {
            let pairs = approvedPairs
            if pairs.isEmpty {
                Text("No approved sessions. When you allow a write from the approval "
                    + "dialog, the pair appears here.")
                    .font(.system(size: 12))
                    .foregroundStyle(Theme.mutedForeground)
                    .fixedSize(horizontal: false, vertical: true)
            } else {
                ForEach(pairs, id: \.id) { pair in
                    LabeledContent {
                        Button("Revoke") {
                            store.revokeMcpWriteApproval(
                                caller: pair.caller, target: pair.target
                            )
                        }
                        .controlSize(.small)
                    } label: {
                        VStack(alignment: .leading, spacing: 1) {
                            Text("\(store.sessionDisplayName(pair.caller)) → "
                                + "\(store.sessionDisplayName(pair.target))")
                                .font(.system(size: 13))
                                .foregroundStyle(Theme.foreground)
                            Text("Can type into the session without asking.")
                                .font(.system(size: 11))
                                .foregroundStyle(Theme.mutedForeground)
                        }
                    }
                }
            }
        } header: {
            SettingsSectionHeader(
                title: "Approved sessions",
                description: "Write approvals you've granted. Each lives until either "
                    + "session is removed."
            )
        }
    }

    private var approvedPairs: [(id: String, caller: String, target: String)] {
        store.mcpWriteApprovals
            .flatMap { caller, targets in
                targets.map { (id: "\(caller)→\($0)", caller: caller, target: $0) }
            }
            .sorted { $0.id < $1.id }
    }

    private var approvedAppsSection: some View {
        Section {
            let approvals = approvedApps
            if approvals.isEmpty {
                Text("No approved Apps. An agent asks before it starts an App panel "
                    + "for the first time.")
                    .font(.system(size: 12))
                    .foregroundStyle(Theme.mutedForeground)
                    .fixedSize(horizontal: false, vertical: true)
            } else {
                ForEach(approvals, id: \.id) { approval in
                    LabeledContent {
                        Button("Revoke") {
                            store.revokeMcpAppOpenApproval(
                                caller: approval.caller, appID: approval.appID
                            )
                        }
                        .controlSize(.small)
                    } label: {
                        VStack(alignment: .leading, spacing: 1) {
                            Text("\(store.sessionDisplayName(approval.caller)) → "
                                + approval.appID)
                                .font(.system(size: 13))
                                .foregroundStyle(Theme.foreground)
                            Text("Can start this App as a companion panel without asking.")
                                .font(.system(size: 11))
                                .foregroundStyle(Theme.mutedForeground)
                        }
                    }
                }
            }
        } header: {
            SettingsSectionHeader(
                title: "Approved Apps",
                description: "App-panel launch approvals you've granted to agent sessions."
            )
        }
    }

    private var approvedApps: [(id: String, caller: String, appID: String)] {
        store.mcpAppOpenApprovals
            .flatMap { caller, apps in
                apps.map { (id: "\(caller)→\($0)", caller: caller, appID: $0) }
            }
            .sorted { $0.id < $1.id }
    }

}

// MARK: - Notifications panel
