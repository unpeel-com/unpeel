import Foundation
import UnpeelShared

/// Current git branch without spawning `git`: parse `.git/HEAD` directly
/// (worktree checkouts have a `.git` FILE pointing at the real gitdir —
/// follow it). Cheap enough to run per project on every bootstrap.
enum GitHeadReader {
    static func currentBranch(repoPath: String) -> String? {
        let gitEntry = repoPath + "/.git"
        var headPath = gitEntry + "/HEAD"
        var isDirectory: ObjCBool = false
        guard FileManager.default.fileExists(atPath: gitEntry, isDirectory: &isDirectory) else {
            return nil
        }
        if !isDirectory.boolValue {
            guard let contents = try? String(contentsOfFile: gitEntry, encoding: .utf8),
                  let gitdir = contents
                      .split(separator: "\n")
                      .first(where: { $0.hasPrefix("gitdir:") })?
                      .dropFirst("gitdir:".count)
                      .trimmingCharacters(in: .whitespaces)
            else { return nil }
            let resolved = gitdir.hasPrefix("/")
                ? gitdir
                : (repoPath as NSString).appendingPathComponent(gitdir)
            headPath = resolved + "/HEAD"
        }
        guard let head = try? String(contentsOfFile: headPath, encoding: .utf8) else {
            return nil
        }
        let trimmed = head.trimmingCharacters(in: .whitespacesAndNewlines)
        guard trimmed.hasPrefix("ref: refs/heads/") else {
            // Detached HEAD: show the short commit instead of nothing.
            return trimmed.isEmpty ? nil : String(trimmed.prefix(7))
        }
        return String(trimmed.dropFirst("ref: refs/heads/".count))
    }
}

extension SessionStatus {
    var remoteStatus: RemoteSessionStatus {
        switch self {
        case .exited: return .exited
        case .starting, .busy, .idle, .attention: return .running
        }
    }
}

extension SessionActivityStatus {
    var remoteActivity: RemoteActivityState {
        switch self {
        case .starting: return .starting
        case .working: return .working
        case .blocked: return .blocked
        case .done: return .done
        case .idle, .exited: return .idle
        }
    }
}

extension Project {
    func remoteProjectSummary(
        folderID: String? = nil,
        parentProjectID: String? = nil,
        isGroup: Bool? = nil,
        colorID: String? = nil,
        pinned: Bool? = nil,
        mcpBlocked effectiveMcpBlocked: Bool? = nil,
        archivedSessionCount: Int? = nil,
        dateSorted: Bool? = nil,
        displaySortOrder: Int? = nil,
        sessionOrder: [String]? = nil
    ) -> RemoteProjectSummary {
        RemoteProjectSummary(
            id: id,
            name: name,
            path: path,
            folderID: folderID,
            parentProjectID: parentProjectID,
            worktreeBranch: worktreeBranch,
            isGroup: isGroup,
            colorID: colorID,
            pinned: pinned,
            gitBranch: GitHeadReader.currentBranch(repoPath: path),
            mcpBlocked: effectiveMcpBlocked ?? mcpBlocked ?? false,
            // The bootstrap passes the DISPLAY rank so Controllers that sort
            // by the field (the TUI does) mirror the array order exactly;
            // the file's own sortOrder is a pre-overlay value that can
            // contradict a drag persisted in project-order.json.
            sortOrder: displaySortOrder ?? sortOrder,
            archivedSessionCount: archivedSessionCount,
            dateSorted: dateSorted,
            sessionOrder: sessionOrder
        )
    }

    func remoteFolderSummary(
        colorID: String? = nil,
        displaySortOrder: Int? = nil
    ) -> RemoteProjectFolderSummary {
        RemoteProjectFolderSummary(
            id: id,
            name: name,
            parentFolderID: parentProjectID,
            colorID: colorID,
            sortOrder: displaySortOrder ?? sortOrder
        )
    }
}

extension Preset {
    func remoteSummary(defaultPresetID: String? = nil) -> RemotePresetSummary {
        let cli = SetupTool.detect(in: command)
        return RemotePresetSummary(
            id: id,
            label: label,
            command: command,
            cliID: cli?.id,
            enabled: enabled,
            quickLaunch: quickLaunch,
            isDefault: defaultPresetID == id,
            tintColorHex: Theme.toolColorHex(forCommand: command)
        )
    }
}

extension SessionEntry {
    func remoteSummary(
        projectID effectiveProjectID: String? = nil,
        unread: Bool = false,
        pinned: Bool = false,
        lastOutputPreview: String? = nil,
        updatedAtUnixMs: Int64? = nil,
        notifyWhenDone: Bool = false,
        terminalBackgroundHex: Int? = nil,
        archived: Bool = false,
        latestAlertBody: String? = nil,
        latestAlertAtUnixMs: Int64? = nil
    ) -> RemoteSessionSummary {
        RemoteSessionSummary(
            id: id,
            projectID: effectiveProjectID ?? projectID,
            activeRuntimeID: isLive ? activeRuntimeID : nil,
            runtimeLaunchPending: isLive && runtimeLaunchPending,
            providerID: SetupTool.detect(in: command)?.id,
            title: label,
            command: command,
            createdAtUnixMs: createdAt,
            ownerPrincipalID: ownerPrincipalID,
            createdByDeviceID: createdByDeviceID,
            sourcePresetID: sourcePresetID,
            // Every Controller gets the Host-computed lifecycle timestamp;
            // it is the sort/age key for Recently updated and never a
            // running manifest heartbeat. Explicit callers may override it.
            updatedAtUnixMs: updatedAtUnixMs
                ?? max(max(createdAt, lifecycleAtMs ?? 0), latestAlertAtUnixMs ?? 0),
            status: status.remoteStatus,
            activity: activityStatus(unread: unread).remoteActivity,
            unread: unread,
            pinned: pinned,
            worktreePath: worktreePath,
            worktreeBranch: worktreeBranch,
            // Kept nil on the wire for compatibility with older controllers;
            // current sessions are flat within their project/group.
            parentSessionID: nil,
            lastOutputPreview: lastOutputPreview,
            notifyWhenDone: notifyWhenDone,
            terminalBackgroundHex: terminalBackgroundHex,
            // Verb support, computed here on the Mac so the phone's session
            // sheet offers exactly what the desktop context menu offers.
            capabilities: ProviderCapabilities.remote(session: self),
            archived: archived,
            // Brand/spinner tint from the Mac's single color table, so a new
            // CLI's color reaches phones without a phone update. An installed
            // Unpeel App's Host-resolved tint wins the same way it does
            // locally.
            spinnerColorHex: (isLive ? activeApp?.spinnerTintColorHex : nil)
                ?? Theme.toolSpinnerColorHex(forCommand: presentationCommand),
            // Installed-App identity as data: id/name/tint resolved on this
            // Host; Controllers cannot know third-party Apps from a compiled
            // catalog.
            activeAppID: isLive ? activeApp?.id : nil,
            activeAppName: isLive ? activeApp?.name : nil,
            activeAppTintHex: isLive ? activeApp?.tintColorHex : nil,
            latestAlertBody: latestAlertBody,
            latestAlertAtUnixMs: latestAlertAtUnixMs
        )
    }
}

enum MobilePaneGroupProjection {
    static func summaries(
        from state: PaneLayoutState
    ) -> [RemotePaneGroupSummary]? {
        let groups = state.groups.compactMap { group -> RemotePaneGroupSummary? in
            let sessionIDs = group.sessionIDs
            guard sessionIDs.count >= 2,
                  let representativeSessionID = group.representativeSessionID,
                  sessionIDs.contains(representativeSessionID)
            else { return nil }
            return RemotePaneGroupSummary(
                id: group.id,
                representativeSessionID: representativeSessionID,
                sessionIDs: sessionIDs
            )
        }
        return groups.isEmpty ? nil : groups
    }

    /// Mobile workspace mux keys name transports; pane layout slots name the
    /// Controller scope.
    static func scopeID(forSelectionKey selectionKey: String?) -> String? {
        guard let selectionKey else { return "local" }
        if selectionKey.hasPrefix("local:") {
            let home = String(selectionKey.dropFirst("local:".count))
            return home.isEmpty ? nil : "workspace:\(home)"
        }
        if selectionKey.hasPrefix("ssh:") {
            let hostID = String(selectionKey.dropFirst("ssh:".count))
            return hostID.isEmpty ? nil : "host:\(hostID)"
        }
        if selectionKey.hasPrefix("host:") {
            return selectionKey.count > "host:".count ? selectionKey : nil
        }
        return nil
    }
}

extension UnpeelStore {
    /// Newest App alert whether or not a later lifecycle event replaced it as
    /// the row's display copy. Read receipts and Recent ordering need the
    /// timestamp of every unread-capable activity source.
    func latestAlertAtMs(for sessionID: String) -> Int64? {
        activityLogEntries.last(where: {
            $0.sessionID == sessionID && $0.kind == .alert
        }).map { Int64(min($0.at, UInt64(Int64.max))) }
    }

    func latestAlertActivity(for sessionID: String) -> ActivityLogEntry? {
        guard let entry = activityLogEntries.last(where: { $0.sessionID == sessionID }),
              entry.kind == .alert,
              entry.message?.isEmpty == false
        else { return nil }
        return entry
    }

}
