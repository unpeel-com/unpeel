import AppKit
import SwiftUI
import UnpeelShared

/// One Host-backed panel for Local, loopback workspaces, SSH, Direct and Link.
/// Its identity changes with the selected scope; in-flight responses are also
/// fenced by RemoteHostRuntime's connection generation.
struct GitSettingsPanel: View {
    @ObservedObject var store: UnpeelStore
    @ObservedObject var runtime: RemoteHostRuntime
    @State private var settings: RemoteGitSettings?
    @State private var hasLoaded = false
    @State private var name = ""
    @State private var email = ""
    @State private var identityEdited = false
    @State private var errorMessage: String?
    @State private var connectionError: String?
    @State private var submitting = false
    @State private var actionTask: Task<Void, Never>?
    @State private var initiatedLoginID: String?
    @State private var openedLoginID: String?
    @State private var disconnectAccount: String?
    @State private var installerSessionID: String?
    @State private var terminalOwner = UUID()

    private var canRead: Bool { runtime.supportsHostOperation(RemoteControlProtocol.gitSettingsReadCapability) }
    private var canWrite: Bool { runtime.supportsHostOperation(RemoteControlProtocol.gitSettingsSetCapability) }
    private var busy: Bool { submitting || settings?.checking == true || settings?.operation?.isRunning == true }
    private var hostName: String { store.selectedScopeDisplayName ?? L10n.t("This Mac") }

    var body: some View {
        VStack(spacing: 0) {
            Form {
                Section {} header: {
                    SettingsPaneHeader(
                        title: L10n.t("Git"),
                        description: L10n.format("GitHub access and Git identity on %@.", hostName)
                    )
                        .padding(.bottom, 4)
                }
                if canRead {
                    if let settings, hasLoaded {
                        githubSection(settings)
                        identitySection(settings)
                        Section {
                            Text(L10n.t("These settings use this Host’s user account. Workspaces running under the same user share Git identity and GitHub sign-in."))
                                .font(.system(size: 12)).foregroundStyle(Theme.mutedForeground)
                            Button(L10n.t("Refresh status")) { perform(.init(action: "refresh")) }
                                .disabled(busy || !canWrite)
                        }
                    } else {
                        Section { ProgressView(L10n.t("Checking Git on this Host…")).controlSize(.small) }
                    }
                } else {
                    Section {
                        Text(L10n.t("Connect to this workspace and update Unpeel on its Host to use Git settings."))
                            .foregroundStyle(Theme.mutedForeground)
                    }
                }
                if let message = errorMessage ?? settings?.operation?.error ?? settings?.error ?? connectionError {
                    Section { Text(message).foregroundStyle(.red).textSelection(.enabled) }
                }
            }
            .formStyle(.grouped)
            .scrollContentBackground(.hidden)

            if let sessionID = installerSessionID {
                VStack(spacing: 0) {
                    HStack {
                        Text(L10n.format("Install GitHub CLI on %@", hostName))
                        Spacer()
                        Button(L10n.t("Check installation")) { perform(.init(action: "refresh")) }.disabled(busy)
                        Button { installerSessionID = nil; updateTerminal() } label: { Image(systemName: "xmark") }
                            .accessibilityLabel(L10n.t("Hide setup terminal"))
                    }.font(.system(size: 12)).padding(12)
                    if let pane = runtime.terminalPane(for: sessionID) {
                        RemoteTerminalPaneHostView(pane: pane, backgroundColor: Theme.terminalBackgroundNSColor, isActive: true)
                            .id(sessionID).frame(height: 230)
                    } else {
                        RemoteTerminalPreparingView(sessionTitle: L10n.t("GitHub CLI setup"), state: runtime.connectionState)
                            .frame(height: 230)
                    }
                }.background(Theme.terminalBackground)
            }
        }
        .task { await poll() }
        .onDisappear {
            actionTask?.cancel()
            runtime.setAuxiliaryTerminalSessions([], owner: terminalOwner)
        }
        .confirmationDialog(L10n.format("Disconnect GitHub on %@?", hostName), isPresented: Binding(
            get: { disconnectAccount != nil }, set: { if !$0 { disconnectAccount = nil } }
        ), titleVisibility: .visible) {
            if let account = disconnectAccount {
                Button(L10n.format("Disconnect @%@", account), role: .destructive) {
                    disconnectAccount = nil
                    perform(.init(action: "disconnectGithub", login: account))
                }
            }
            Button(L10n.t("Cancel"), role: .cancel) { disconnectAccount = nil }
        } message: {
            Text(L10n.t("Removes this account from GitHub CLI on this Host. Other workspaces using the same server user are affected. It does not revoke the authorization on GitHub."))
        }
    }

    @ViewBuilder
    private func githubSection(_ settings: RemoteGitSettings) -> some View {
        Section(L10n.t("GitHub")) {
            if !settings.githubCliInstalled {
                Text(L10n.t("GitHub CLI is not installed on this Host."))
                if let command = settings.githubCliInstallCommand {
                    Text(command).font(.system(size: 12, design: .monospaced)).textSelection(.enabled)
                    Button(L10n.t("Install GitHub CLI…")) { install(command) }
                        .disabled(submitting || runtime.snapshot?.projects.isEmpty != false)
                    if runtime.snapshot?.projects.isEmpty != false {
                        Text(L10n.t("Add a folder to this workspace to open a setup terminal."))
                            .font(.system(size: 12)).foregroundStyle(Theme.mutedForeground)
                    }
                }
                Link(L10n.t("GitHub CLI installation instructions"), destination: URL(string: "https://cli.github.com/")!)
            } else {
                HStack {
                    VStack(alignment: .leading, spacing: 4) {
                        Text(
                            settings.github.state == "connected"
                                ? L10n.format("Connected as @%@", settings.github.login ?? "")
                                : settings.github.state == "error"
                                    ? L10n.t("GitHub connection needs attention")
                                    : L10n.t("Not connected")
                        )
                        if settings.github.environmentToken {
                            Text(L10n.t("Authentication comes from an environment token on this Host."))
                                .font(.system(size: 12)).foregroundStyle(Theme.mutedForeground)
                        }
                    }
                    Spacer()
                    if busy { ProgressView().controlSize(.small) }
                    if let login = settings.github.login, !settings.github.environmentToken {
                        Button(L10n.t("Disconnect…")) { disconnectAccount = login }.disabled(busy || !canWrite)
                    }
                    if settings.github.state != "connected", !settings.github.environmentToken {
                        Button(L10n.t("Sign in with GitHub")) {
                            let action = RemoteGitSettingsAction(action: "connectGithub")
                            initiatedLoginID = action.requestID
                            perform(action)
                        }.disabled(busy || !canWrite)
                    }
                }
                Text(L10n.t("GitHub credentials stay on this Host. This sign-in flow does not send them to Unpeel’s service."))
                    .font(.system(size: 12)).foregroundStyle(Theme.mutedForeground)
                if settings.github.state == "connected", !settings.github.environmentToken {
                    switch settings.github.credentialStorage {
                    case "keyring":
                        Text(L10n.t("Stored in this Host’s system credential store."))
                            .font(.system(size: 12)).foregroundStyle(Theme.mutedForeground)
                    case "file":
                        Text(L10n.t("GitHub CLI is storing the token in a plain-text file on this Host. Configure a system credential store and sign in again to use it."))
                            .font(.system(size: 12)).foregroundStyle(Theme.mutedForeground)
                    default:
                        Text(L10n.t("This GitHub CLI version does not report credential storage. Check it on the Host with gh auth status."))
                            .font(.system(size: 12)).foregroundStyle(Theme.mutedForeground)
                    }
                }
                if settings.github.state == "connected" {
                    if settings.github.gitConfigured {
                        Label(L10n.t("Git HTTPS access configured"), systemImage: "checkmark.circle")
                            .foregroundStyle(Theme.mutedForeground)
                    } else {
                        Button(L10n.t("Configure Git access")) { perform(.init(action: "setupGit")) }
                            .disabled(busy || !canWrite || !settings.gitInstalled)
                    }
                }
                if let operation = settings.operation, operation.isRunning, operation.action == "connectGithub" {
                    if let code = operation.userCode, operation.githubAuthorizationURL != nil {
                        VStack(alignment: .leading, spacing: 8) {
                            Text(code).font(.system(size: 24, weight: .semibold, design: .monospaced)).textSelection(.enabled)
                            Text(L10n.format("Paste this code into GitHub in your browser to authorize %@.", hostName))
                                .font(.system(size: 12)).foregroundStyle(Theme.mutedForeground)
                            Button(L10n.t("Copy Code and Open GitHub")) { openAuthorization(operation) }
                        }
                    } else {
                        Text(L10n.t("Starting GitHub sign-in…")).foregroundStyle(Theme.mutedForeground)
                    }
                    Button(L10n.t("Cancel sign-in")) { perform(.init(action: "cancelGithub", operationID: operation.id)) }
                        .disabled(submitting || !canWrite)
                }
            }
        }
    }

    private func identitySection(_ settings: RemoteGitSettings) -> some View {
        Section(L10n.t("Git identity")) {
            if settings.gitInstalled {
                TextField(L10n.t("Name"), text: Binding(get: { name }, set: { name = $0; identityEdited = true }))
                TextField(L10n.t("Email"), text: Binding(get: { email }, set: { email = $0; identityEdited = true }))
                Text(L10n.t("Used for new commits. Repository-specific Git configuration can override these defaults."))
                    .font(.system(size: 12)).foregroundStyle(Theme.mutedForeground)
                Button(L10n.t("Save identity")) { perform(.init(action: "setIdentity", name: name, email: email)) }
                    .disabled(busy || !canWrite || (name == settings.name && email == settings.email))
            } else {
                Text(L10n.t("Git is not installed on this Host.")).foregroundStyle(Theme.mutedForeground)
            }
        }
    }

    @MainActor private func poll() async {
        let scope = store.selectedHostScope.paneScopeID
        while !Task.isCancelled, store.selectedHostScope.paneScopeID == scope {
            if canRead {
                do {
                    let value = try await runtime.gitSettings()
                    guard !Task.isCancelled, store.selectedHostScope.paneScopeID == scope else { return }
                    connectionError = nil
                    accept(value)
                } catch is CancellationError {
                    if Task.isCancelled || store.selectedHostScope.paneScopeID != scope { return }
                }
                catch { if !Task.isCancelled, store.selectedHostScope.paneScopeID == scope { connectionError = error.localizedDescription } }
            }
            do { try await Task.sleep(for: .seconds(1)) } catch { return }
        }
    }

    @MainActor private func accept(_ value: RemoteGitSettings) {
        if !identityEdited && !value.checking && value.operation?.isRunning != true {
            name = value.name
            email = value.email
        }
        settings = value
        if !value.checking { hasLoaded = true }
        if let operation = value.operation, operation.id == initiatedLoginID,
           openedLoginID != operation.id, operation.githubAuthorizationURL != nil {
            openedLoginID = operation.id
            openAuthorization(operation)
        }
    }

    @MainActor private func openAuthorization(_ operation: RemoteGitSettings.Operation) {
        guard let url = operation.githubAuthorizationURL, let code = operation.userCode else { return }
        NSPasteboard.general.clearContents()
        NSPasteboard.general.setString(code, forType: .string)
        if !NSWorkspace.shared.open(url) {
            errorMessage = L10n.t("Could not open GitHub. Open github.com/login/device and enter the code above.")
        }
    }

    @MainActor private func perform(_ action: RemoteGitSettingsAction) {
        let scope = store.selectedHostScope.paneScopeID
        submitting = true
        errorMessage = nil
        actionTask = Task { @MainActor in
            defer { submitting = false }
            do {
                guard !Task.isCancelled, store.selectedHostScope.paneScopeID == scope else { return }
                try await runtime.setGitSettings(action)
                guard !Task.isCancelled, store.selectedHostScope.paneScopeID == scope else { return }
                if action.action == "setIdentity" { identityEdited = false }
                let value = try await runtime.gitSettings()
                guard !Task.isCancelled, store.selectedHostScope.paneScopeID == scope else { return }
                accept(value)
            } catch is CancellationError { }
            catch { if !Task.isCancelled, store.selectedHostScope.paneScopeID == scope { errorMessage = error.localizedDescription } }
        }
    }

    @MainActor private func install(_ command: String) {
        guard let projectID = runtime.snapshot?.projects.first?.id else { return }
        let scope = store.selectedHostScope.paneScopeID
        submitting = true
        actionTask = Task { @MainActor in
            defer { submitting = false }
            do {
                guard !Task.isCancelled, store.selectedHostScope.paneScopeID == scope else { return }
                let sessionID = try await runtime.createSession(projectID: projectID, command: command, selectOnCreate: false)
                guard !Task.isCancelled, store.selectedHostScope.paneScopeID == scope else { return }
                installerSessionID = sessionID
                updateTerminal()
            } catch { if !Task.isCancelled, store.selectedHostScope.paneScopeID == scope { errorMessage = error.localizedDescription } }
        }
    }

    @MainActor private func updateTerminal() {
        runtime.setAuxiliaryTerminalSessions(installerSessionID.map { Set([$0]) } ?? [], owner: terminalOwner)
    }
}
