//
//  BrowserSettingsPanel.swift
//  UnpeelNative
//
//  Extracted from SettingsView.swift — Settings ▸ Browser use panel.
//

import SwiftUI

/// Settings home for the Unpeel Browser MCP: engine status, options, and the
/// app-wide Browser Access. Access is the single `browser_default_access` field
/// in app-state.json (read per call by the `__browser_mcp__` host gate) — one
/// global on/off, no per-session override.
struct BrowserSettingsPanel: View {
    @ObservedObject var store: UnpeelStore

    /// The Host's published engine state (`serve.json.browserEngine`,
    /// written by the workspace worker that installs and verifies the
    /// engine — see docs/agents/browser-mcp.md). nil until read.
    @State private var engine: HostBrowserEngineStatus?

    var body: some View {
        VStack(alignment: .leading, spacing: 0) {
            Form {
                Section {} header: {
                    SettingsPaneHeader(
                        title: "Browser MCP",
                        description: "Browser MCP lets an agent session drive a real browser — "
                            + "open pages, click, fill forms, and take screenshots. By default, "
                            + "sessions in a project share one browser window and logins, with "
                            + "each agent safely pinned to its own tab."
                    )
                    .padding(.bottom, 4)
                }

                statusSection
                defaultSection
                approvalsSection
                optionsSection
                siteRulesSection
            }
            .formStyle(.grouped)
            .scrollContentBackground(.hidden)
        }
        .task { await refreshEngineStatus() }
        .onAppear {
            allowedDomainsDraft = store.browserSettings.allowedDomains
            executablePathDraft = store.browserSettings.executablePath
        }
    }

    /// Text-field drafts commit on submit/focus-loss instead of per keystroke,
    /// so we don't rewrite app-state.json on every character.
    @State private var allowedDomainsDraft = ""
    @State private var executablePathDraft = ""

    private var optionsSection: some View {
        Section {
            LabeledContent {
                Toggle(
                    "",
                    isOn: Binding(
                        get: { store.browserSettings.headed },
                        set: { value in store.updateBrowserSettings { $0.headed = value } }
                    )
                )
                .toggleStyle(.switch)
                .labelsHidden()
                .controlSize(.small)
            } label: {
                VStack(alignment: .leading, spacing: 1) {
                    Text("Show browser window")
                        .font(.system(size: 13))
                        .foregroundStyle(Theme.foreground)
                    Text(store.browserSettings.headed
                        ? "You see what the agent does, live."
                        : "The browser runs in the background — screenshots still work.")
                        .font(.system(size: 11))
                        .foregroundStyle(Theme.mutedForeground)
                        .fixedSize(horizontal: false, vertical: true)
                }
            }

            if store.browserSettings.headed {
                LabeledContent {
                    Toggle(
                        "",
                        isOn: Binding(
                            get: { store.browserSettings.showCursor },
                            set: { value in
                                store.updateBrowserSettings { $0.showCursor = value }
                            }
                        )
                    )
                    .toggleStyle(.switch)
                    .labelsHidden()
                    .controlSize(.small)
                } label: {
                    VStack(alignment: .leading, spacing: 1) {
                        Text("Show agent cursor")
                            .font(.system(size: 13))
                            .foregroundStyle(Theme.foreground)
                        Text(store.browserSettings.showCursor
                            ? "A pointer glides to whatever the agent clicks or fills, so "
                                + "you can follow along. Adds a short beat before each action."
                            : "Actions happen instantly, with no visible pointer.")
                            .font(.system(size: 11))
                            .foregroundStyle(Theme.mutedForeground)
                            .fixedSize(horizontal: false, vertical: true)
                    }
                }
            }

            LabeledContent {
                Picker(
                    "",
                    selection: Binding(
                        get: { store.browserSettings.keepsProjectProfile },
                        set: { keep in
                            store.updateBrowserSettings {
                                $0.profileMode = keep ? "project" : "session"
                            }
                        }
                    )
                ) {
                    Text("Separate per session").tag(false)
                    Text("Shared project window").tag(true)
                }
                .labelsHidden()
                .pickerStyle(.menu)
                .fixedSize()
            } label: {
                VStack(alignment: .leading, spacing: 1) {
                    Text("Browser scope")
                        .font(.system(size: 13))
                        .foregroundStyle(Theme.foreground)
                    Text(store.browserSettings.keepsProjectProfile
                        ? "One window per project; every session gets its own tab and all share "
                            + "cookies and logins. Other projects stay separate."
                        : "Each session gets a separate browser; its cookies and logins vanish "
                            + "when that browser closes.")
                        .font(.system(size: 11))
                        .foregroundStyle(Theme.mutedForeground)
                        .fixedSize(horizontal: false, vertical: true)
                }
            }

            if store.browserSettings.keepsProjectProfile {
                LabeledContent {
                    Button("Clear…") { store.clearBrowserProfiles() }
                        .controlSize(.small)
                } label: {
                    VStack(alignment: .leading, spacing: 1) {
                        Text("Clear project browser data")
                            .font(.system(size: 13))
                            .foregroundStyle(Theme.foreground)
                        Text("Deletes saved project profiles (logins, cookies). Open project "
                            + "windows keep theirs until every session tab closes.")
                            .font(.system(size: 11))
                            .foregroundStyle(Theme.mutedForeground)
                            .fixedSize(horizontal: false, vertical: true)
                    }
                }
            }

            LabeledContent {
                TextField("Auto-detect Chrome", text: $executablePathDraft)
                    .textFieldStyle(.roundedBorder)
                    .font(.system(size: 11, design: .monospaced))
                    .frame(width: 220)
                    .onSubmit { commitExecutablePath() }
            } label: {
                VStack(alignment: .leading, spacing: 1) {
                    Text("Browser app")
                        .font(.system(size: 13))
                        .foregroundStyle(Theme.foreground)
                    Text("Path to a Chromium-based browser (Chrome, Brave, Edge, Arc). "
                        + "Leave empty to auto-detect. Press Return to apply.")
                        .font(.system(size: 11))
                        .foregroundStyle(Theme.mutedForeground)
                        .fixedSize(horizontal: false, vertical: true)
                }
            }
        } header: {
            SettingsSectionHeader(
                title: "Options",
                description: "Applied to the agent's next browser action — no restart needed."
            )
        }
    }

    private var siteRulesSection: some View {
        Section {
            VStack(alignment: .leading, spacing: 6) {
                TextField("example.com, *.example.com", text: $allowedDomainsDraft)
                    .textFieldStyle(.roundedBorder)
                    .font(.system(size: 12, design: .monospaced))
                    .onSubmit { commitAllowedDomains() }
                Text(store.browserSettings.allowedDomains.trimmingCharacters(in: .whitespaces).isEmpty
                    ? "All sites allowed. Add comma-separated domains to restrict browsing — "
                        + "wildcards like *.example.com work, and the browser itself blocks "
                        + "everything else (pages, scripts, requests). Press Return to apply."
                    : "Browsing is restricted to the listed domains, enforced inside the "
                        + "browser engine. Clear the field and press Return to allow all sites.")
                    .font(.system(size: 11))
                    .foregroundStyle(Theme.mutedForeground)
                    .fixedSize(horizontal: false, vertical: true)
            }
        } header: {
            SettingsSectionHeader(
                title: "Site access",
                description: "Limit which websites agents can reach."
            )
        }
    }

    private func commitAllowedDomains() {
        let value = allowedDomainsDraft.trimmingCharacters(in: .whitespacesAndNewlines)
        store.updateBrowserSettings { $0.allowedDomains = value }
    }

    private func commitExecutablePath() {
        let value = executablePathDraft.trimmingCharacters(in: .whitespacesAndNewlines)
        store.updateBrowserSettings { $0.executablePath = value }
    }


    /// The Host installs the engine itself (`~/.unpeel/browser/bin`) and
    /// publishes the result in `serve.json.browserEngine`. Healthy = one
    /// quiet line; anything else explains itself and names the fix
    /// (`unpeel browser install` on the Host).
    @ViewBuilder
    private var statusSection: some View {
        Section {
            switch engine?.state {
            case "ready":
                VStack(alignment: .leading, spacing: 2) {
                    Text("Browser engine ready")
                        .font(.system(size: 13, weight: .semibold))
                    Text(verbatim: "agent-browser \(engine?.version ?? "") at "
                        + "\(engine?.path ?? "~/.unpeel/browser/bin/agent-browser"), "
                        + "installed and verified by the Host.")
                        .font(.system(size: 11))
                        .foregroundStyle(Theme.mutedForeground)
                        .fixedSize(horizontal: false, vertical: true)
                }
            case "installing":
                VStack(alignment: .leading, spacing: 2) {
                    Text("Browser engine installing…")
                        .font(.system(size: 13, weight: .semibold))
                    Text(verbatim: "The Host is downloading and verifying agent-browser "
                        + "\(engine?.version ?? ""). Browser tools become available "
                        + "as soon as it finishes.")
                        .font(.system(size: 11))
                        .foregroundStyle(Theme.mutedForeground)
                        .fixedSize(horizontal: false, vertical: true)
                }
            case "disabled":
                VStack(alignment: .leading, spacing: 2) {
                    Text("Browser engine install disabled")
                        .font(.system(size: 13, weight: .semibold))
                        .foregroundStyle(.orange)
                    Text(verbatim: "This Host runs with UNPEEL_BROWSER_ENGINE_INSTALL=0, so it "
                        + "never installs the engine itself. Run "
                        + "`unpeel browser install` on the Host, or set "
                        + "UNPEEL_AGENT_BROWSER_BIN to an engine binary.")
                        .font(.system(size: 11))
                        .foregroundStyle(Theme.mutedForeground)
                        .fixedSize(horizontal: false, vertical: true)
                }
            case "failed":
                VStack(alignment: .leading, spacing: 2) {
                    Text("Browser engine install failed")
                        .font(.system(size: 13, weight: .semibold))
                        .foregroundStyle(.orange)
                    Text(verbatim: (engine?.error ?? "unknown error")
                        + " — run `unpeel browser install` on the Host to retry "
                        + "(it re-verifies the pinned agent-browser "
                        + "\(engine?.version ?? "") by sha256).")
                        .font(.system(size: 11))
                        .foregroundStyle(Theme.mutedForeground)
                        .fixedSize(horizontal: false, vertical: true)
                }
            default:
                VStack(alignment: .leading, spacing: 2) {
                    Text("Browser engine status unknown")
                        .font(.system(size: 13, weight: .semibold))
                        .foregroundStyle(.orange)
                    Text(verbatim: "The Host service has not published an engine state yet "
                        + "(no serve.json). Once it is running it installs the engine "
                        + "on its own; `unpeel browser install` does the same by hand.")
                        .font(.system(size: 11))
                        .foregroundStyle(Theme.mutedForeground)
                        .fixedSize(horizontal: false, vertical: true)
                }
            }
        }
    }

    /// The app-wide default every session gets unless individually overridden.
    /// Default off: browser automation is opt-in.
    private var defaultSection: some View {
        Section {
            LabeledContent {
                Picker(
                    "",
                    selection: Binding(
                        get: { store.browserDefaultAccess },
                        set: { store.setDefaultBrowserAccess($0) }
                    )
                ) {
                    ForEach(BrowserAccess.allCases) { access in
                        Text(access.label).tag(access)
                    }
                }
                .labelsHidden()
                .pickerStyle(.menu)
                .fixedSize()
            } label: {
                VStack(alignment: .leading, spacing: 1) {
                    Text("Browser access")
                        .font(.system(size: 13))
                        .foregroundStyle(Theme.foreground)
                    Text(store.browserDefaultAccess.detail)
                        .font(.system(size: 11))
                        .foregroundStyle(Theme.mutedForeground)
                        .fixedSize(horizontal: false, vertical: true)
                }
            }
        } footer: {
            Text("Off applies immediately to running sessions. Enabling reaches an agent "
                + "when it starts in a newly configured terminal.")
                .font(.system(size: 11))
                .foregroundStyle(Theme.mutedForeground)
        }
    }

    /// Remembered per-session approvals, shown only in Ask mode (mirrors
    /// Settings ▸ Computer).
    @ViewBuilder
    private var approvalsSection: some View {
        if store.browserDefaultAccess == .ask, !store.browserApprovals.isEmpty {
            Section("Approved sessions") {
                ForEach(store.browserApprovals, id: \.self) { sessionID in
                    LabeledContent {
                        Button("Revoke") {
                            store.revokeBrowserApproval(sessionID: sessionID)
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

    /// Read the worker's published state; cheap, so re-read every few
    /// seconds while the pane is open until it settles on ready (an install
    /// finishes within seconds of a fresh start).
    private func refreshEngineStatus() async {
        while !Task.isCancelled {
            engine = HostBrowserEngineStatus.read(
                at: LaunchConfig.unpeelDir.appendingPathComponent("serve.json"))
            if engine?.state == "ready" { return }
            try? await Task.sleep(nanoseconds: 3_000_000_000)
        }
    }
}

/// `serve.json.browserEngine` as the Host publishes it (additive;
/// docs/agents/serve.md). Decoded leniently: any missing field is nil, an
/// unknown state renders as "unknown".
struct HostBrowserEngineStatus: Equatable {
    let state: String
    let version: String?
    let path: String?
    let error: String?

    static func read(at url: URL) -> HostBrowserEngineStatus? {
        guard let data = try? Data(contentsOf: url),
              let json = try? JSONSerialization.jsonObject(with: data) as? [String: Any],
              let engine = json["browserEngine"] as? [String: Any],
              let state = engine["state"] as? String
        else { return nil }
        return HostBrowserEngineStatus(
            state: state,
            version: engine["version"] as? String,
            path: engine["path"] as? String,
            error: engine["error"] as? String
        )
    }
}

// MARK: - Nav row (.settings-row, SettingsView.svelte:211-271)
