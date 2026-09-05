//
//  MCPApprovalCenter.swift
//  UnpeelNative
//
//  Unified pending-approval queue behind the ask-mode bridge routes
//  (/mcp/approve-write, /mcp/approve-browser, /mcp/approve-computer,
//  /mcp/approve-app-open). Each
//  route's handler keeps its own fast paths and remembered-answer store; what
//  they share is everything after that: a FIFO queue of `PendingMcpApproval`s
//  with stable ids, coalescing of identical requests, and an `answer` API that
//  any surface can call — the in-session desktop overlay, or a paired
//  controller via POST /mobile/approvals/answer. First answer wins; the rest
//  see the request disappear.
//
//  The desktop prompt is an in-pane overlay on the Session this grant is
//  about (write: the destination; otherwise the asking Session), never a
//  floating window and never `NSAlert.runModal()`. A nested modal run loop
//  inside a main-actor job stalls every queued main-actor hop — including
//  mobile bootstrap — so a pending prompt used to sever the phone connection
//  whenever the app had no key window. Nothing on this path may block the
//  main actor.
//

import AppKit
import Foundation

/// One pending ask-mode approval request, unified across the three kinds.
struct PendingMcpApproval: Identifiable, Equatable {
    enum Kind: String {
        /// Inter-session `send_text`/`send_keys`/`report` (caller → target pair).
        case write
        /// First browser action of a session under Ask.
        case browser
        /// First computer action of a session under Ask.
        case computer
        /// First launch of one installed App by this session.
        case appOpen = "app-open"
    }

    let id: String
    let kind: Kind
    let callerSessionID: String
    /// Write approvals only: the session being written into.
    let targetSessionID: String?
    /// App-open approvals only: the installed App being launched.
    let targetAppID: String?
    let targetAppName: String?
    let requestedAt: Date

    /// Session that should show the in-pane prompt and the attention badge.
    /// Write grants present on the destination so the user sees where input
    /// would land; other kinds have no destination and present on the caller.
    /// A missing/unknown destination falls back to the caller.
    func presentationSessionID(knownIDs: Set<String>) -> String {
        if let target = targetSessionID, knownIDs.contains(target) {
            return target
        }
        return callerSessionID
    }
}

/// Overlay pending MCP approvals onto the displayed Session tree as
/// attention. Pure so the sidebar, activity menu, and tests share one rule.
enum McpApprovalAttention {
    static func applying(
        to session: SessionEntry,
        pendingSessionIDs: Set<String>
    ) -> SessionEntry {
        guard session.isLive, pendingSessionIDs.contains(session.id) else {
            return session
        }
        var next = session
        next.status = .attention
        return next
    }

    static func applying(
        to nodes: [ProjectNode],
        pendingSessionIDs: Set<String>
    ) -> [ProjectNode] {
        guard !pendingSessionIDs.isEmpty else { return nodes }
        func mapNode(_ node: ProjectNode) -> ProjectNode {
            ProjectNode(
                project: node.project,
                sessions: node.sessions.map {
                    applying(to: $0, pendingSessionIDs: pendingSessionIDs)
                },
                worktrees: node.worktrees.map(mapNode)
            )
        }
        return nodes.map(mapNode)
    }

    static func applying(
        to sessions: [String: SessionEntry],
        pendingSessionIDs: Set<String>
    ) -> [String: SessionEntry] {
        guard !pendingSessionIDs.isEmpty else { return sessions }
        return sessions.mapValues {
            applying(to: $0, pendingSessionIDs: pendingSessionIDs)
        }
    }
}

extension UnpeelStore {
    /// Reconcile the Rust Host's approval queue into the existing native
    /// prompt surface. The callback is a snapshot, not an effect: an approval
    /// answered by a phone simply disappears here on the next generation.
    func reconcileHostApprovals(_ presented: [PlatformPresentedApproval]) {
        let incomingIDs = Set(presented.map(\.id))
        pendingMcpApprovals.removeAll { approval in
            hostOwnedMcpApprovalIDs.contains(approval.id)
                && !incomingIDs.contains(approval.id)
        }
        hostOwnedMcpApprovalIDs.formIntersection(incomingIDs)
        hostMcpApprovalMessages = hostMcpApprovalMessages.filter {
            incomingIDs.contains($0.key)
        }
        hostMcpApprovalAnswersInFlight.formIntersection(incomingIDs)

        let existingIDs = Set(pendingMcpApprovals.map(\.id))
        var didReveal = false
        for item in presented {
            hostMcpApprovalMessages[item.id] = (item.title, item.body)
            guard !existingIDs.contains(item.id),
                  let kind = PendingMcpApproval.Kind(rawValue: item.kind)
            else {
                hostOwnedMcpApprovalIDs.insert(item.id)
                continue
            }
            let approval = PendingMcpApproval(
                id: item.id,
                kind: kind,
                callerSessionID: item.callerSessionID,
                targetSessionID: item.targetSessionID,
                targetAppID: nil,
                targetAppName: nil,
                requestedAt: Date(
                    timeIntervalSince1970: Double(item.requestedAtUnixMs) / 1_000
                )
            )
            hostOwnedMcpApprovalIDs.insert(item.id)
            pendingMcpApprovals.append(approval)
            notifyMcpApprovalRequested(approval)
            if !didReveal {
                revealSessionForMcpApproval(approval)
                didReveal = true
            }
        }
    }

    /// Answer a pending approval by id. Returns false when the id is no
    /// longer pending (already answered elsewhere) — remote callers surface
    /// that as "handled on another device" instead of an error.
    @discardableResult
    func answerMcpApproval(id: String, approved: Bool) -> Bool {
        guard let index = pendingMcpApprovals.firstIndex(where: { $0.id == id }) else {
            return false
        }
        if hostOwnedMcpApprovalIDs.contains(id) {
            guard hostMcpApprovalAnswersInFlight.insert(id).inserted else { return true }
            Task { @MainActor [weak self] in
                guard let self else { return }
                var backend: NativeRemoteBackend?
                do {
                    let local = try NativeRemoteBackend(
                        localGatewayHome: LaunchConfig.unpeelDir.standardizedFileURL.path,
                        expectedHostID: self.localHostID,
                        requireHostService: true
                    )
                    backend = local
                    _ = try await local.bootstrap()
                    _ = try await local.answerApproval(id: id, approved: approved)
                    let answered = self.pendingMcpApprovals.first { $0.id == id }
                    self.pendingMcpApprovals.removeAll { $0.id == id }
                    if approved, answered?.kind == .computer {
                        // The user is engaged right now — if required TCC
                        // grants are missing, the approval they just gave
                        // leads straight into a failing first action, so
                        // chain into the grant prompt on this Mac.
                        self.checkComputerPermissionsAfterApproval()
                    }
                    self.hostOwnedMcpApprovalIDs.remove(id)
                    self.hostMcpApprovalMessages.removeValue(forKey: id)
                } catch {
                    ToastCenter.shared.show(
                        "Couldn’t answer the approval",
                        systemImage: "exclamationmark.triangle"
                    )
                }
                await backend?.close()
                self.hostMcpApprovalAnswersInFlight.remove(id)
            }
            return true
        }
        // Every pending approval is Host-owned since the Swift Host
        // retirement; an id that is not is a stale row the next
        // reconciliation removes.
        pendingMcpApprovals.remove(at: index)
        return false
    }

    var mcpApprovalAttentionSessionIDs: Set<String> {
        let known = mcpApprovalKnownSessionIDs
        return Set(pendingMcpApprovals.map {
            $0.presentationSessionID(knownIDs: known)
        })
    }

    func mcpApprovalPresentationSessionID(_ approval: PendingMcpApproval) -> String {
        approval.presentationSessionID(knownIDs: mcpApprovalKnownSessionIDs)
    }

    func sessionNeedsMcpApprovalAttention(_ sessionID: String) -> Bool {
        mcpApprovalAttentionSessionIDs.contains(sessionID)
    }

    func pendingMcpApproval(forSessionID sessionID: String) -> PendingMcpApproval? {
        let known = mcpApprovalKnownSessionIDs
        return pendingMcpApprovals.first {
            $0.presentationSessionID(knownIDs: known) == sessionID
        }
    }

    func pendingMcpApprovalCount(forSessionID sessionID: String) -> Int {
        let known = mcpApprovalKnownSessionIDs
        return pendingMcpApprovals.filter {
            $0.presentationSessionID(knownIDs: known) == sessionID
        }.count
    }

    /// Bring Unpeel forward and open the Session that should show the prompt,
    /// unless that Session is already on screen (main panes or the project
    /// sidebar). The in-pane overlay and sidebar attention badge are the
    /// findability path; this just avoids leaving the grant on a hidden row.
    func revealSessionForMcpApproval(_ approval: PendingMcpApproval) {
        NSApp.activate(ignoringOtherApps: true)
        closeSettings()
        let sessionID = mcpApprovalPresentationSessionID(approval)
        guard mcpApprovalKnownSessionIDs.contains(sessionID) else { return }
        if mcpApprovalSessionIsVisible(sessionID) { return }
        selectedSessionID = sessionID
    }

    func mcpApprovalSessionIsVisible(_ sessionID: String) -> Bool {
        if selectedSessionID == sessionID { return true }
        if sessionIsInProjectSidebar(sessionID) { return true }
        guard let selected = selectedSessionID,
              let group = validatedPaneGroup(containingSession: selected)
        else { return false }
        return group.panes.contains { $0.content.sessionID == sessionID }
    }

    /// Prompt copy shared by the in-session overlay and remote controllers.
    /// Resolved at render time so titles follow session renames.
    func mcpApprovalMessage(_ approval: PendingMcpApproval) -> (title: String, body: String) {
        if let presented = hostMcpApprovalMessages[approval.id] {
            return presented
        }
        switch approval.kind {
        case .write:
            let target = approval.targetSessionID.map(sessionDisplayName) ?? "another session"
            return (
                "Allow “\(sessionDisplayName(approval.callerSessionID))” to type into “\(target)”?",
                "An agent session is asking to send input to another session. "
                    + "Allowing remembers this pair until either session "
                    + "is removed — manage approvals in Settings ▸ Sessions use."
            )
        case .browser:
            return (
                "Allow “\(sessionDisplayName(approval.callerSessionID))” to use a browser?",
                "The agent gets its own isolated browser window — separate profile, no "
                    + "access to your logins or tabs. Allowing remembers this session "
                    + "until it is removed — manage approvals in Settings ▸ Browser."
            )
        case .computer:
            return (
                "Allow “\(sessionDisplayName(approval.callerSessionID))” to control this Mac?",
                "The agent will be able to read app windows and click and type into them "
                    + "in the background — your real apps, including anything sensitive "
                    + "they show. It won't move your cursor or steal focus. Allowing "
                    + "remembers this session until it is removed — manage approvals in "
                    + "Settings ▸ Computer."
            )
        case .appOpen:
            let appName = approval.targetAppName ?? approval.targetAppID ?? "an App"
            return (
                "Allow “\(sessionDisplayName(approval.callerSessionID))” to open \(appName)?",
                "The App runs in a hosted companion process and can appear as a panel "
                    + "beside this agent. Allowing remembers this App for this session."
            )
        }
    }

    /// Human-readable session name for the approval prompt and the Settings
    /// approved-pairs list. Falls back to a short id for sessions this
    /// instance doesn't know (e.g. already removed).
    func sessionDisplayName(_ sessionID: String) -> String {
        if let session = displaySessionsByID[sessionID]
            ?? sessionsByID[sessionID]
            ?? remoteSessionsByID[sessionID]
        {
            let label = session.label.trimmingCharacters(in: .whitespaces)
            if !label.isEmpty { return label }
        }
        return String(sessionID.prefix(8))
    }
}
