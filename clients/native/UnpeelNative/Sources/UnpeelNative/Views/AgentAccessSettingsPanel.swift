//
//  AgentAccessSettingsPanel.swift
//  UnpeelNative
//
//  Settings ▸ Agent access: what agents may do through Unpeel's tools.
//

import SwiftUI

/// One page for the unified unpeel MCP server's policies: the Sessions
/// domain (open reads, approval-controlled writes, worktrees, gallery,
/// remembered pairs) and the Browser domain (engine, access default,
/// approvals, options, site rules). Which agents *have* the server is the
/// Agents page; this page is what they may do with it. Each half follows
/// its feature gate (`sessionsMcp`, `browserMcp`).
struct AgentAccessSettingsPanel: View {
    @ObservedObject var store: UnpeelStore
    @ObservedObject private var localization = LocalizationStore.shared

    var body: some View {
        VStack(alignment: .leading, spacing: 0) {
            Form {
                Section {} header: {
                    SettingsPaneHeader(
                        title: L10n.t("Agent access"),
                        description: L10n.t(
                            "What connected agents may do through Unpeel's tools: read and "
                                + "type into your other sessions, drive a real browser, prepare "
                                + "worktrees, and publish to the gallery. Reads are open; every write "
                                + "follows the policies here. This is a cooperative policy for agents "
                                + "using Unpeel's tools, not a sandbox.")
                    )
                    .padding(.bottom, 4)
                }

                if UnpeelFeatureFlags.isEnabled(.sessionsMcp) {
                    SessionsAccessSections(store: store)
                }
                if UnpeelFeatureFlags.isEnabled(.browserMcp) {
                    BrowserAccessSections(store: store)
                }
                if !UnpeelFeatureFlags.isEnabled(.sessionsMcp), !UnpeelFeatureFlags.isEnabled(.browserMcp) {
                    Section {
                        Text(L10n.t(
                            "Turn on Sessions use or Browser use under Features to configure agent access."))
                            .font(.system(size: 13))
                            .foregroundStyle(Theme.mutedForeground)
                    }
                }
            }
            .formStyle(.grouped)
            .scrollContentBackground(.hidden)
        }
    }
}
