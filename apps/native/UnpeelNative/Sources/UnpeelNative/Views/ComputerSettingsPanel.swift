//
//  ComputerSettingsPanel.swift
//  UnpeelNative
//
//  Settings ▸ Computer (experimental, gated by ExperimentalFeature.computerUse):
//  the control surface for the Computer MCP domain — engine + daemon + macOS
//  permission status, the app-wide access mode (Off / Ask each session /
//  Allow), and the remembered per-session approvals with revoke. All writes
//  go through UnpeelStore into app-state.json; the unified MCP server
//  re-reads them per call, so every change here applies live (no restart
//  banners). Permissions are probed natively (the app itself is the TCC
//  identity — the embedded cua-driver daemon inherits its grants) and the
//  Grant buttons raise the correctly-attributed system prompts.
//

import AppKit
import SwiftUI

struct ComputerSettingsPanel: View {
    @ObservedObject var store: UnpeelStore

    /// Resolved engine path, mirroring computer_mcp.rs resolve_engine_binary.
    /// nil while probing; empty string when nothing was found.
    @State private var enginePath: String?
    /// Native TCC probe rows (Accessibility / Screen Recording).
    @State private var permissions: [ComputerPermissions.Row] = ComputerPermissions.probe()
    @State private var daemonRunning = false

    var body: some View {
        VStack(alignment: .leading, spacing: 0) {
            Form {
                Section {} header: {
                    SettingsPaneHeader(
                        title: "Computer MCP",
                        description: "Computer MCP lets an agent session control this Mac's "
                            + "apps in the background: read a window's UI elements, take "
                            + "screenshots, click, and type — without moving your cursor or "
                            + "stealing focus (you see a separate overlay cursor). Unlike "
                            + "the browser, this is your real desktop — the agent sees "
                            + "whatever the target app shows, so by default each session "
                            + "asks you once before its first action. Computer MCP is "
                            + "development-only while its privileged engine is being isolated. "
                            + "The approval prompt coordinates agents using MCP; it is not a "
                            + "sandbox against commands running as your macOS user."
                    )
                    .padding(.bottom, 4)
                }

                accessSection
                approvalsSection
                statusSection
            }
            .formStyle(.grouped)
            .scrollContentBackground(.hidden)
        }
        .task { await probe() }
        .onReceive(
            NotificationCenter.default.publisher(
                for: NSApplication.didBecomeActiveNotification
            )
        ) { _ in
            // Re-probe when the user comes back from System Settings so a
            // fresh grant shows without reopening the panel (the engine
            // manager restarts the daemon on the same signal).
            Task { await probe() }
        }
    }

    // MARK: - Status (engine, daemon, permissions)

    /// The engine ships inside the app — no engine row when healthy ("it
    /// should simply just work"). Only actionable states render: a missing
    /// engine (broken install), a stopped daemon, and the TCC grant rows.
    private var statusSection: some View {
        Section("Status") {
            if enginePath == "" {
                VStack(alignment: .leading, spacing: 2) {
                    Text("Computer-use engine missing")
                        .font(.system(size: 13, weight: .semibold))
                        .foregroundStyle(.orange)
                    Text("This Unpeel install is missing its bundled computer-use engine "
                        + "(cua-driver), so computer tools are unavailable. Reinstall "
                        + "Unpeel from unpeel.com/download/mac to fix it. (Dev builds can "
                        + "place a build at ~/.unpeel/computer/bin/cua-driver or on PATH.)")
                        .font(.system(size: 11))
                        .foregroundStyle(Theme.mutedForeground)
                        .fixedSize(horizontal: false, vertical: true)
                }
            }

            if store.computerDefaultAccess != .off, enginePath?.isEmpty == false,
               !daemonRunning {
                Text("The engine daemon is not running yet — it starts automatically while "
                    + "computer access is enabled. If this persists, toggle Computer access "
                    + "off and on.")
                    .font(.system(size: 11))
                    .foregroundStyle(.orange)
                    .fixedSize(horizontal: false, vertical: true)
            }

            ForEach(permissions, id: \.name) { permission in
                LabeledContent {
                    HStack(spacing: 8) {
                        Text(permission.granted ? "Granted" : "Not granted")
                            .font(.system(size: 12))
                            .foregroundStyle(
                                permission.granted ? Theme.mutedForeground : .orange
                            )
                        if !permission.granted {
                            Button("Grant…") {
                                ComputerPermissions.request(permission.name)
                            }
                            .controlSize(.small)
                        }
                    }
                } label: {
                    Text(permission.name)
                        .font(.system(size: 13))
                        .foregroundStyle(Theme.foreground)
                }
            }
        }
    }

    // MARK: - Access mode

    private var accessSection: some View {
        Section {
            Picker(
                selection: Binding(
                    get: { store.computerDefaultAccess },
                    set: { store.setDefaultComputerAccess($0) }
                )
            ) {
                ForEach(ComputerAccess.allCases) { access in
                    Text(access.label).tag(access)
                }
            } label: {
                VStack(alignment: .leading, spacing: 1) {
                    Text("Computer access")
                        .font(.system(size: 13))
                        .foregroundStyle(Theme.foreground)
                    Text(store.computerDefaultAccess.detail)
                        .font(.system(size: 11))
                        .foregroundStyle(Theme.mutedForeground)
                        .fixedSize(horizontal: false, vertical: true)
                }
            }
            .pickerStyle(.menu)
        } footer: {
            Text("Off applies immediately to running sessions and stops the engine. "
                + "Enabling reaches an agent when it starts in a newly configured terminal.")
                .font(.system(size: 11))
                .foregroundStyle(Theme.mutedForeground)
        }
    }

    // MARK: - Remembered approvals (Ask mode)

    @ViewBuilder
    private var approvalsSection: some View {
        if store.computerDefaultAccess == .ask, !store.computerApprovals.isEmpty {
            Section("Approved sessions") {
                ForEach(store.computerApprovals, id: \.self) { sessionID in
                    LabeledContent {
                        Button("Revoke") {
                            store.revokeComputerApproval(sessionID: sessionID)
                        }
                        .controlSize(.small)
                    } label: {
                        Text(store.sessionDisplayName(sessionID))
                            .font(.system(size: 13))
                            .foregroundStyle(Theme.foreground)
                    }
                }
            }
        }
    }

    // MARK: - Probes

    private func probe() async {
        permissions = ComputerPermissions.probe()
        daemonRunning = ComputerEngineManager.shared.isRunning
        let path = await Task.detached(priority: .utility) {
            ComputerPermissions.resolveEngine()
        }.value
        enginePath = path ?? ""
    }
}
