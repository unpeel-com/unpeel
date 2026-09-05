//
//  SelectedHostScope.swift
//  UnpeelNative
//
//  The local-execution safety boundary for the future Host picker. This is
//  deliberately smaller than SessionBackend: it exists now so every local
//  spawn is guarded before remote scope becomes a product surface.
//

import Foundation

enum SelectedHostScope: Equatable, Sendable {
    case local
    case remote(hostID: String)
    /// Another LOCAL workspace scoped through the loopback gateway
    /// (workspaces-unification phase 2). It is NOT this instance's home:
    /// local execution stays refused exactly as in remote scope, and every
    /// verb rides the selected Host connection. Keyed by the normalized
    /// workspace home — the one identity every local workspace has, running
    /// or not, registered or the implicit default.
    case localWorkspace(home: String, name: String)

    var permitsLocalExecution: Bool {
        self == .local
    }

    /// True whenever the selected scope is a workspace on THIS Mac — this
    /// instance's own Local scope, or another local workspace reached over
    /// the loopback gateway. Filesystem/state verbs (Add Project, the
    /// project context menu) run against a home ON this machine, so they are
    /// valid here; a true `.remote(hostID:)` (paired/SSH) scope is NOT a local
    /// machine and stays a pure client with no filesystem verbs.
    ///
    /// Session HOSTING still keys off `permitsLocalExecution` alone: a
    /// `.localWorkspace` never spawns a local session under this instance's
    /// home — its sessions ride the gateway exactly like a remote Host.
    var isLocalMachine: Bool {
        switch self {
        case .local, .localWorkspace:
            return true
        case .remote:
            return false
        }
    }

    /// Multiple panes are a Controller rendering capability, not a Host or
    /// workspace capability. Every selected scope uses the same pane model;
    /// only the terminal transport behind each pane changes.
    var supportsSessionPanes: Bool {
        true
    }

    /// Stable key for this Controller window's presentation state. Names are
    /// deliberately excluded: renaming a workspace must not orphan its pane
    /// layout. This key is local to the Controller's own `UNPEEL_HOME`.
    var paneScopeID: String {
        switch self {
        case .local:
            return "local"
        case let .localWorkspace(home, _):
            return "workspace:\(home)"
        case let .remote(hostID):
            return "host:\(hostID)"
        }
    }

    /// The `UNPEEL_HOME` that a local-against-home filesystem/state verb must
    /// target: `nil` for `.local` (meaning this instance's own home — callers
    /// use their existing `LaunchConfig.appStateFile`/`AppDefaults.shared`),
    /// and the scoped workspace's home for `.localWorkspace`. `nil` for a
    /// true remote scope too — those verbs never run there.
    var scopedLocalHome: String? {
        guard case .localWorkspace(let home, _) = self else { return nil }
        return home
    }

    var remoteHostID: String? {
        guard case .remote(let hostID) = self else { return nil }
        return hostID
    }

    var localWorkspaceHome: String? {
        guard case .localWorkspace(let home, _) = self else { return nil }
        return home
    }

    var localWorkspaceName: String? {
        guard case .localWorkspace(_, let name) = self else { return nil }
        return name
    }

    /// Additive field in the session-host launch JSON. Rust defaults a missing
    /// value to local for compatibility, and rejects remote_controller before
    /// creating session artifacts or installing provider hooks.
    var sessionLaunchWireValue: String {
        switch self {
        case .local: "local"
        case .remote, .localWorkspace: "remote_controller"
        }
    }
}
