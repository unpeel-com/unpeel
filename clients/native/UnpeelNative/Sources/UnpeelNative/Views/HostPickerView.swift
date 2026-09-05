//
//  HostPickerView.swift
//  UnpeelNative
//
//  Workspace-adding and remote-pairing sheets for the unified Workspaces
//  list (WorkspacesSettingsPanel renders the rows). AddWorkspaceSheet adds a
//  workspace of any kind — a fresh local workspace, a nearby/code-paired
//  Host, or an SSH Host. Bonjour makes Hosts discoverable; the one-time code
//  remains the authority until the Host has an explicit
//  approve-this-Controller handshake. (Re-homed from Settings ▸ Remote
//  2026-08-17 — inbound paired phones stay there.)
//

import AppKit
import SwiftUI
import UnpeelShared

/// A selected Host mints the authority; this Mac is only the short-lived LAN
/// courier for the phone's sealed request/response.
struct RemoteHostPairingSheet: View {
    @ObservedObject var store: UnpeelStore
    let hostName: String
    @Environment(\.dismiss) private var dismiss
    @State private var now = Date()

    private var expiry: Date? {
        store.remoteHostPairingPayload.map {
            Date(timeIntervalSince1970: TimeInterval($0.expiresAtUnixMs) / 1000)
        }
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
                    Text("Add an iPhone or iPad to \(hostName)")
                        .font(.system(size: 20, weight: .semibold))
                    Text("This Mac forwards a one-time sealed exchange to the remote workspace.")
                        .font(.system(size: 12))
                        .foregroundStyle(Theme.mutedForeground)
                }
                Spacer()
                Button("Done") { dismiss() }
                    .keyboardShortcut(.cancelAction)
            }

            if let error = store.remoteHostPairingError {
                Text(error)
                    .font(.system(size: 12))
                    .foregroundStyle(.red)
                    .fixedSize(horizontal: false, vertical: true)
            }

            if store.remoteHostPairingCompleted {
                HStack(spacing: 14) {
                    Image(systemName: "checkmark.circle.fill")
                        .font(.system(size: 34))
                        .foregroundStyle(.green)
                    VStack(alignment: .leading, spacing: 5) {
                        Text("Device added to \(hostName)")
                            .font(.system(size: 13, weight: .semibold))
                        Text("The phone now has its own revocable Direct and Link credentials for \(hostName).")
                            .font(.system(size: 12))
                            .foregroundStyle(Theme.mutedForeground)
                        Button("Add Another iPhone or iPad") {
                            store.beginRemoteHostPairing()
                        }
                        .controlSize(.small)
                    }
                    Spacer()
                }
                .padding(.vertical, 12)
            } else {
                HStack(alignment: .top, spacing: 18) {
                    PairingQRCodeView(payload: store.remoteHostPairingCode)
                        .frame(width: 184, height: 184)

                    VStack(alignment: .leading, spacing: 10) {
                        Text("Scan this code in Unpeel on the phone.")
                            .font(.system(size: 13, weight: .semibold))
                        Text(
                            "The phone only needs to reach this Mac for the one-time sealed exchange. "
                                + "The remote workspace creates the credentials; this Mac forwards the sealed response without opening it."
                        )
                            .font(.system(size: 12))
                            .foregroundStyle(Theme.mutedForeground)
                            .fixedSize(horizontal: false, vertical: true)

                        if store.remoteHostPairingPayload != nil {
                            Text(expiresInText)
                                .font(.system(size: 12, weight: .medium))
                                .monospacedDigit()
                                .foregroundStyle(Theme.mutedForeground)
                        } else if store.remoteHostPairingError == nil {
                            ProgressView("Creating invitation…")
                                .controlSize(.small)
                        }

                        HStack(spacing: 8) {
                            Button(
                                store.remoteHostPairingPayload == nil
                                    ? "Generate QR Code" : "Refresh QR Code"
                            ) {
                                store.beginRemoteHostPairing()
                            }
                            .controlSize(.small)

                            if let code = store.remoteHostPairingCode {
                                Button("Copy Pairing Code") {
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

            Text("After pairing, the phone connects to \(hostName) itself—Direct when reachable, otherwise through Unpeel Link if enabled.")
                .font(.system(size: 11))
                .foregroundStyle(Theme.mutedForeground)
        }
        .padding(24)
        .frame(width: 540)
        .onAppear {
            if store.remoteHostPairingPayload == nil,
               !store.remoteHostPairingCompleted {
                store.beginRemoteHostPairing()
            }
        }
        .onDisappear {
            store.cancelRemoteHostPairing()
        }
        .onReceive(Timer.publish(every: 1, on: .main, in: .common).autoconnect()) { tick in
            now = tick
            if let expiry,
               expiry <= tick,
               !store.remoteHostPairingCompleted {
                store.beginRemoteHostPairing()
            }
        }
    }
}

/// One sheet for every kind of new workspace (workspaces-unification): a
/// fresh local workspace on this Mac, a nearby/code-paired Host, or an SSH
/// Host. The "On This Mac" and "Remote" lists both open it, preselecting the
/// relevant method; release builds (no remote Host picker) get the local
/// method only and still switch to the new workspace in this window.
struct AddWorkspaceSheet: View {
    enum Method: String, CaseIterable, Identifiable {
        case thisMac = "On This Mac"
        case pairing = "Nearby or code"
        case ssh = "SSH"

        var id: String { rawValue }
    }

    @ObservedObject var store: UnpeelStore
    @ObservedObject var hosts: RemoteHostStore
    @StateObject private var browser: NearbyHostBrowser
    @Environment(\.dismiss) private var dismiss

    @State private var method: Method
    @State private var workspaceName = ""
    /// App color for a new local workspace, written into its own defaults
    /// suite at create (same key Settings ▸ Workspaces' per-row picker sets).
    @State private var workspaceTint: AppTint = .none
    @State private var selectedCandidateID: String?
    @State private var pairingCode = ""
    @State private var sshTarget = ""
    @State private var sshSecret = ""
    @State private var sshInstallSuggested = false
    @State private var working = false
    @State private var errorMessage: String?
    @State private var operationTask: Task<Void, Never>?

    init(store: UnpeelStore, hosts: RemoteHostStore, initialMethod: Method) {
        self.store = store
        self.hosts = hosts
        _method = State(initialValue:
            RemoteHostFeature.pickerEnabled ? initialMethod : .thisMac
        )
        _browser = StateObject(
            wrappedValue: NearbyHostBrowser(excludingHostID: store.localHostID)
        )
    }

    var body: some View {
        VStack(alignment: .leading, spacing: 18) {
            HStack {
                VStack(alignment: .leading, spacing: 4) {
                    Text(RemoteHostFeature.pickerEnabled ? "Add Workspace" : "New Workspace")
                        .font(.system(size: 20, weight: .semibold))
                    Text(RemoteHostFeature.pickerEnabled
                        ? "A new workspace on this Mac, or one on another machine you control."
                        : "A new, separate workspace on this Mac.")
                        .font(.system(size: 12))
                        .foregroundStyle(Theme.mutedForeground)
                }
                Spacer()
                Button("Cancel") { cancelAndDismiss() }
                    .keyboardShortcut(.cancelAction)
            }

            if RemoteHostFeature.pickerEnabled {
                Picker("Kind", selection: $method) {
                    ForEach(Method.allCases) { method in
                        Text(method.rawValue).tag(method)
                    }
                }
                .pickerStyle(.segmented)
            }

            switch method {
            case .thisMac:
                thisMacContent
            case .pairing:
                pairingContent
            case .ssh:
                sshContent
            }
        }
        .padding(24)
        .frame(width: 540)
        .onAppear {
            if method == .pairing { browser.start() }
        }
        .onDisappear {
            operationTask?.cancel()
            operationTask = nil
            browser.stop()
        }
        .onChange(of: method) { method in
            errorMessage = nil
            sshInstallSuggested = false
            if method == .pairing { browser.start() } else { browser.stop() }
        }
        .onChange(of: sshTarget) { _ in
            sshInstallSuggested = false
            errorMessage = nil
        }
        .onChange(of: sshSecret) { _ in
            sshInstallSuggested = false
            errorMessage = nil
        }
        .onChange(of: browser.candidates) { candidates in
            if let selectedCandidateID,
               !candidates.contains(where: { $0.hostID == selectedCandidateID }) {
                self.selectedCandidateID = nil
            }
        }
    }

    @ViewBuilder
    private var thisMacContent: some View {
        VStack(alignment: .leading, spacing: 8) {
            Text("Workspace name")
                .font(.system(size: 12, weight: .semibold))
            TextField("Work", text: $workspaceName)
                .textFieldStyle(.roundedBorder)
                .onSubmit(createLocalWorkspace)
            Text(
                "Starts blank, fully separate from this workspace: its own "
                    + "sessions, projects, presets, and settings, and it pairs "
                    + "with your phone as its own Mac."
            )
                .font(.system(size: 11))
                .foregroundStyle(Theme.mutedForeground)
                .fixedSize(horizontal: false, vertical: true)
        }

        VStack(alignment: .leading, spacing: 8) {
            Text("Color")
                .font(.system(size: 12, weight: .semibold))
            HStack(spacing: 12) {
                ForEach(AppTint.allCases) { tint in
                    AppTintSwatch(tint: tint, isSelected: workspaceTint == tint) {
                        workspaceTint = tint
                    }
                }
                Spacer(minLength: 0)
            }
        }

        errorView

        HStack {
            Text(
                "Created workspaces appear in the workspace switcher; renaming never moves their data."
            )
                .font(.system(size: 10))
                .foregroundStyle(Theme.mutedForeground)
            Spacer()
            actionButton(title: "Create", action: createLocalWorkspace)
                .disabled(
                    working
                        || workspaceName
                            .trimmingCharacters(in: .whitespacesAndNewlines)
                            .isEmpty
                )
        }
    }

    @ViewBuilder
    private var pairingContent: some View {
        VStack(alignment: .leading, spacing: 8) {
            Text("Nearby")
                .font(.system(size: 12, weight: .semibold))
            if case .unavailable(let message) = browser.state {
                VStack(alignment: .leading, spacing: 5) {
                    Text("Nearby discovery is unavailable")
                        .font(.system(size: 11, weight: .medium))
                    Text(message)
                        .font(.system(size: 10))
                        .foregroundStyle(Theme.mutedForeground)
                    Text("You can still paste the pairing code below.")
                        .font(.system(size: 10))
                        .foregroundStyle(Theme.mutedForeground)
                }
                .frame(maxWidth: .infinity, minHeight: 54, alignment: .leading)
            } else if browser.candidates.isEmpty {
                HStack(spacing: 8) {
                    ProgressView().controlSize(.small)
                    Text(
                        "Workspaces on your network appear here. On the other machine, "
                            + "run `unpeel serve` (or open Unpeel)."
                    )
                        .foregroundStyle(Theme.mutedForeground)
                }
                .frame(maxWidth: .infinity, minHeight: 54, alignment: .leading)
            } else {
                VStack(spacing: 4) {
                    candidateRow(
                        name: "Use pairing code only",
                        detail: nil,
                        icon: "qrcode",
                        selected: selectedCandidateID == nil
                    ) { selectedCandidateID = nil }
                    ForEach(browser.candidates) { candidate in
                        candidateRow(
                            name: candidate.name,
                            detail: candidate.hostID,
                            icon: "server.rack",
                            selected: selectedCandidateID == candidate.hostID
                        ) { selectedCandidateID = candidate.hostID }
                    }
                }
            }
        }

        VStack(alignment: .leading, spacing: 8) {
            Text("Pairing code")
                .font(.system(size: 12, weight: .semibold))
            Text(
                "On the other machine, run `unpeel pair` — or `unpeel pair --serve` "
                    + "to start serving at the same time — then enter the code it shows here. "
                    + "For another Mac running the Unpeel app, its code is in "
                    + "Settings ▸ Remote Control."
            )
                .font(.system(size: 11))
                .foregroundStyle(Theme.mutedForeground)
            TextField("UNPEEL:1:…", text: $pairingCode)
                .textFieldStyle(.roundedBorder)
                .font(.system(size: 11, design: .monospaced))
        }

        errorView

        HStack {
            Text("Codes work once and expire after 5 minutes.")
                .font(.system(size: 10))
                .foregroundStyle(Theme.mutedForeground)
            Spacer()
            actionButton(title: "Pair", action: completePairing)
                .disabled(
                    working
                        || pairingCode.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty
                )
        }
    }

    @ViewBuilder
    private var sshContent: some View {
        VStack(alignment: .leading, spacing: 8) {
            Text("SSH destination")
                .font(.system(size: 12, weight: .semibold))
            TextField("user@host or SSH config alias", text: $sshTarget)
                .textFieldStyle(.roundedBorder)
                .font(.system(size: 12, design: .monospaced))
            Text("Unpeel uses your system SSH configuration, keys, agent, host-key policy, ProxyJump, and VPN. Put ports and identity files in ~/.ssh/config.")
                .font(.system(size: 10))
                .foregroundStyle(Theme.mutedForeground)
                .fixedSize(horizontal: false, vertical: true)
        }

        VStack(alignment: .leading, spacing: 8) {
            Text("Password or API key (optional)")
                .font(.system(size: 12, weight: .semibold))
            SecureField("Leave empty for SSH keys or agent", text: $sshSecret)
                .textFieldStyle(.roundedBorder)
            Text("Saved in this Mac’s Keychain so Unpeel can reopen this workspace. It is never stored on the remote machine or passed in command arguments.")
                .font(.system(size: 10))
                .foregroundStyle(Theme.mutedForeground)
        }

        VStack(alignment: .leading, spacing: 7) {
            Text("On the remote machine")
                .font(.system(size: 11, weight: .semibold))
            Text(sshInstallSuggested
                ? "Unpeel could not be started. Install it with the same SSH connection, or copy the command as a fallback:"
                : "If Unpeel is missing, Connect will offer to install it. Manual fallback:")
                .font(.system(size: 10))
                .foregroundStyle(Theme.mutedForeground)
            HStack {
                Text("curl -fsSL https://unpeel.com/install.sh | sh")
                    .font(.system(size: 10, design: .monospaced))
                    .textSelection(.enabled)
                Spacer()
                Button("Copy") {
                    NSPasteboard.general.clearContents()
                    NSPasteboard.general.setString(
                        "curl -fsSL https://unpeel.com/install.sh | sh",
                        forType: .string
                    )
                }
                .controlSize(.small)
            }
        }
        .padding(10)
        .background(
            RoundedRectangle(cornerRadius: 8, style: .continuous)
                .fill(sshInstallSuggested ? Theme.accent.opacity(0.10) : Theme.hoverRow)
        )

        errorView

        HStack {
            Text(
                "Supports ordinary SSH and interactive-only managed shells automatically.\n"
                    + "No service needs to run on the box — Unpeel connects over SSH on demand. "
                    + "For phone access and notifications while disconnected, also run "
                    + "`unpeel serve` there."
            )
                .font(.system(size: 10))
                .foregroundStyle(Theme.mutedForeground)
            Spacer()
            actionButton(
                title: sshInstallSuggested ? "Install & Connect" : "Connect",
                action: completeSSH
            )
                .disabled(
                    working
                        || sshTarget.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty
                )
        }
    }

    @ViewBuilder
    private var errorView: some View {
        if let errorMessage {
            Text(errorMessage)
                .font(.system(size: 11))
                .foregroundStyle(.red)
                .fixedSize(horizontal: false, vertical: true)
        }
    }

    private func candidateRow(
        name: String,
        detail: String?,
        icon: String,
        selected: Bool,
        action: @escaping () -> Void
    ) -> some View {
        Button(action: action) {
            HStack {
                Image(systemName: icon)
                VStack(alignment: .leading, spacing: 1) {
                    Text(name)
                    if let detail {
                        Text(detail)
                            .font(.system(size: 10, design: .monospaced))
                            .foregroundStyle(Theme.mutedForeground)
                    }
                }
                Spacer()
                if selected {
                    Image(systemName: "checkmark.circle.fill")
                        .foregroundStyle(Theme.accent)
                }
            }
            .contentShape(Rectangle())
        }
        .buttonStyle(.plain)
        .padding(9)
        .background(
            RoundedRectangle(cornerRadius: 8, style: .continuous)
                .fill(selected ? Theme.accent.opacity(0.10) : Theme.hoverRow)
        )
    }

    private func actionButton(title: String, action: @escaping () -> Void) -> some View {
        Button(action: action) {
            if working {
                ProgressView().controlSize(.small)
            } else {
                Text(title)
            }
        }
        .keyboardShortcut(.defaultAction)
    }

    /// Create the registry workspace, then land the user in it exactly as a
    /// workspace-switcher selection would: rescope this window over the
    /// loopback gateway. Opening a workspace in another window remains an
    /// explicit action; creation must never launch a second app instance.
    private func createLocalWorkspace() {
        guard !working else { return }
        let name = workspaceName.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !name.isEmpty else { return }
        errorMessage = nil
        do {
            let record = try UnpeelWorkspaceRegistry.create(name: name)
            if workspaceTint != .none {
                // The new workspace's own defaults suite — the same write
                // Settings ▸ Workspaces' per-row picker performs, minus the
                // reload ping (the instance isn't running yet).
                AppDefaults.suite(forUnpeelHome: record.home)
                    .set(workspaceTint.rawValue, forKey: UnpeelStore.nativeAppTintKey)
                NotificationCenter.default.post(
                    name: .unpeelWorkspaceTintChanged, object: nil
                )
            }
            NotificationCenter.default.post(
                name: .unpeelWorkspaceListChanged,
                object: nil
            )
            store.selectLocalWorkspace(record)
            dismiss()
        } catch {
            errorMessage = error.localizedDescription
        }
    }

    private func completePairing() {
        guard !working else { return }
        working = true
        errorMessage = nil
        let code = pairingCode
        let expectedHostID = selectedCandidateID
        operationTask = Task { @MainActor in
            defer { working = false }
            do {
                let record = try await hosts.pair(
                    code: code,
                    expectedHostID: expectedHostID
                )
                try Task.checkCancellation()
                NotificationCenter.default.post(
                    name: .unpeelWorkspaceListChanged,
                    object: nil
                )
                store.selectHost(record.hostID, forceReconnect: true)
                dismiss()
            } catch is CancellationError {
                return
            } catch {
                errorMessage = error.localizedDescription
            }
        }
    }

    private func completeSSH() {
        guard !working else { return }
        working = true
        errorMessage = nil
        let target = sshTarget
        let secret = sshSecret
        operationTask = Task { @MainActor in
            defer { working = false }
            do {
                if sshInstallSuggested {
                    _ = try await store.installAndAddSSHHost(target: target, secret: secret)
                } else {
                    _ = try await store.addSSHHost(target: target, secret: secret)
                }
                try Task.checkCancellation()
                NotificationCenter.default.post(
                    name: .unpeelWorkspaceListChanged,
                    object: nil
                )
                dismiss()
            } catch is CancellationError {
                return
            } catch let error as SSHHostSetupError {
                if case .connection = error {
                    sshInstallSuggested = true
                    errorMessage = "Unpeel is not available on this SSH Host yet, or could not be started. Install it and connect again."
                } else {
                    errorMessage = error.localizedDescription
                }
            } catch {
                errorMessage = error.localizedDescription
            }
        }
    }

    private func cancelAndDismiss() {
        operationTask?.cancel()
        operationTask = nil
        dismiss()
    }
}
