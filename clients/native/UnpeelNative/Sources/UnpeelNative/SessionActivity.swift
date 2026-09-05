//
//  SessionActivity.swift
//  UnpeelNative
//
//  Native port of the hook-driven session activity semantics:
//
//  - apps/desktop/src-tauri/src/session_activity.rs — the hook latch
//    (`hook_seen`), the busy/idle/attention transitions, the 5-minute
//    hook idle timeout, and the output-growth re-arm of that deadline.
//  - apps/desktop/src/lib/stores/sessionState.ts — once a session is
//    hook-owned ("explicitLifecycle"), output/input heuristics stop driving
//    its busy/idle state entirely.
//  - apps/desktop/src/lib/sessionUnread.ts — the pending-unread
//    reconciliation that the store layers on top.
//

import Foundation

/// Per-session hook-driven activity state. Sessions only get an entry after
/// their first authoritative hook event. Terminal output can maintain a
/// hook-owned deadline, but it never originates Busy: hookless agents and
/// ordinary terminals remain visually neutral.
@MainActor
final class SessionActivityEngine {
    /// HOOK_IDLE_TIMEOUT_MS (session_activity.rs:16): hook Start events are
    /// coarse; a busy hook session falls back to idle only after 5 silent
    /// minutes. Output growth keeps re-arming the deadline mid-turn
    /// (refresh_hook_busy_deadline, :225-249).
    static let hookIdleTimeout: TimeInterval = 5 * 60

    /// Stop-distrust guard (codex): codex fires agent-turn-complete Stops for
    /// internal sub-turns of one long run, so a Stop is not proof the work
    /// ended. Growth observed after the grace (past the turn's trailing render
    /// burst) but inside the window re-arms busy; the bounded window keeps
    /// later user scroll repaints from faking busy on a finished session.
    static let stopRearmGrace: TimeInterval = 5
    static let stopRearmWindow: TimeInterval = 90

    struct Entry {
        var state: SessionStatus = .idle
        /// Latched on the first hook event (transition_state, :446-449).
        /// From then on hooks are the only trusted busy/idle signal; raw
        /// output volume must not flip the state.
        var hookSeen = false
        /// Busy idle-timeout deadline; nil while idle/attention.
        var deadlineAt: Date?
        var lastOutputSize: UInt64?
        /// Receipt time of the latest hook event. Runtime-generation resets
        /// use this to preserve a fast event from the newly launched agent
        /// while discarding the preceding generation's stale hook latch.
        var lastHookEventAt: Date?
        /// Whether that latest hook semantically completed a turn. Kept
        /// separately from `state == .idle`: latch-only SessionStart/HookSeen
        /// is idle too, but must clear a prior generation's completion bit.
        var lastHookCompletedTurn = false
        var lastHookEventName: String?
        var lastHookWasLatchOnly = false
        /// Generation carried by the latest accepted owned-hook event. Nil is
        /// a legacy/custom hook without Host provenance.
        var lastHookRuntimeGeneration: UInt64?
        /// A Start/UserPromptSubmit proven to belong to this generation has
        /// established ownership, so a later legacy Stop can be associated
        /// with the same runtime. Stops never establish ownership themselves.
        var ownedRuntimeGeneration: UInt64?
        /// Turn opener from an unversioned hook. When the manifest generation
        /// edge arrives after a fast hook, its receipt time may bind this turn
        /// to the new launch; a lone late Stop cannot.
        var legacyTurnStartedAt: Date?
        var legacyTurnStartEventName: String?
        /// When the latest Stop/StopFailure landed; the stop-distrust guard
        /// only re-arms busy inside [grace, window] after this instant.
        var stoppedAt: Date?
    }

    private(set) var entries: [String: Entry] = [:]

    /// Whether this Session can acquire authoritative hook-owned activity.
    /// Runtime observation may identify/tint a hookless tool, but output and
    /// screen repaints are not lifecycle authority and must never start an
    /// animated Busy indicator.
    ///
    /// An installed Unpeel App (`hasActiveApp`) is hook-capable by
    /// construction: its status reporter posts lifecycle events to the hook
    /// port, and the Host only stamps App identity from an installed
    /// manifest. Detection alone still animates nothing — with no reported
    /// events the session simply stays neutral.
    static func hasHookActivityAuthority(
        launchCommand: String,
        activeRuntimeID: String?,
        hasActiveApp: Bool = false
    ) -> Bool {
        if hasActiveApp { return true }
        let launchTool = SetupTool.detect(in: launchCommand)
        if launchTool?.usesLifecycleHooks == true { return true }
        let observedTool = activeRuntimeID.flatMap { SetupTool(rawValue: $0) }
        return observedTool?.usesLifecycleHooks == true
    }

    /// The session's hook-owned state, or nil when the session has never
    /// produced a hook event. Hookless agents and ordinary terminals remain
    /// idle rather than inferring semantic work from screen changes.
    func hookOwnedState(_ sessionID: String) -> SessionStatus? {
        guard let entry = entries[sessionID], entry.hookSeen else { return nil }
        return entry.state
    }

    enum Transition {
        case busy
        case idle
        case attention
        case none
    }

    /// apply_hook_event (session_activity.rs:306-335):
    /// Start/UserPromptSubmit → busy, Stop/StopFailure → idle,
    /// PermissionRequest → attention. Every event latches `hookSeen`.
    /// `latchOnly` records the event without a state change (used for
    /// PermissionRequest from AskUserQuestion, which the UI ignores —
    /// App.svelte:666-670).
    @discardableResult
    func applyHookEvent(
        sessionID: String,
        hookEventName: String,
        latchOnly: Bool = false,
        runtimeGeneration: UInt64? = nil,
        now: Date = Date()
    ) -> Transition {
        var entry = entries[sessionID] ?? Entry()
        entry.hookSeen = true
        entry.lastHookEventAt = now
        entry.lastHookCompletedTurn = !latchOnly
            && (hookEventName == "Stop" || hookEventName == "StopFailure")
        entry.lastHookEventName = hookEventName
        entry.lastHookWasLatchOnly = latchOnly
        entry.lastHookRuntimeGeneration = runtimeGeneration
        defer { entries[sessionID] = entry }

        if latchOnly {
            return .none
        }

        switch hookEventName {
        case "Start", "UserPromptSubmit":
            if let runtimeGeneration {
                entry.ownedRuntimeGeneration = runtimeGeneration
                entry.legacyTurnStartedAt = nil
                entry.legacyTurnStartEventName = nil
            } else {
                entry.legacyTurnStartedAt = now
                entry.legacyTurnStartEventName = hookEventName
            }
            entry.state = .busy
            entry.deadlineAt = now.addingTimeInterval(Self.hookIdleTimeout)
            entry.stoppedAt = nil
            return .busy
        case "Stop", "StopFailure":
            entry.state = .idle
            entry.deadlineAt = nil
            entry.stoppedAt = now
            return .idle
        case "PermissionRequest":
            entry.state = .attention
            entry.deadlineAt = nil
            entry.stoppedAt = nil
            // Re-baseline output tracking: the prompt render that triggered
            // this request must not count as "agent resumed" on the next
            // sweep. Only output growth *after* the user answers should clear
            // attention (see noteOutputAndSweep).
            entry.lastOutputSize = nil
            return .attention
        default:
            return .none
        }
    }

    /// Per-scan-tick maintenance, the native equivalent of the 1s sweep in
    /// start_background_tasks (session_activity.rs:131-219):
    /// - output growth while hook-busy re-arms the 5-minute deadline
    ///   (refresh_hook_busy_deadline),
    /// - a passed deadline drops the session to idle (Timeout source).
    /// Returns true when the sweep transitioned the session to idle.
    @discardableResult
    func noteOutputAndSweep(
        sessionID: String,
        outputSize: UInt64,
        allowAttentionClearFromOutput: Bool = true,
        distrustStops: Bool = false,
        now: Date = Date()
    ) -> Bool {
        guard var entry = entries[sessionID] else { return false }
        defer { entries[sessionID] = entry }

        let previous = entry.lastOutputSize
        entry.lastOutputSize = outputSize
        let grew = previous.map { outputSize != $0 } ?? false

        // Output growth on an attention session means the agent resumed after
        // the user answered: their keystrokes echo and the agent's reply both
        // append to output.bin. Without this, attention only clears on a later
        // hook event — and a single missed/dropped Stop leaves the dot stuck
        // forever, with none of the busy state's idle-timeout safety net. Treat
        // resumed output as busy; the busy idle-timeout below then carries it
        // the rest of the way to idle.
        //
        // Grok paints the question UI to the terminal before the user answers,
        // so callers can disable this path and let UserPromptSubmit clear it.
        if entry.state == .attention {
            if allowAttentionClearFromOutput, grew {
                entry.state = .busy
                entry.deadlineAt = now.addingTimeInterval(Self.hookIdleTimeout)
            }
            return false
        }

        if entry.state == .busy, grew {
            entry.deadlineAt = now.addingTimeInterval(Self.hookIdleTimeout)
        }
        if entry.state == .busy, let deadline = entry.deadlineAt, deadline <= now {
            entry.state = .idle
            entry.deadlineAt = nil
            return true
        }

        // Stop-distrust guard: a hook-idle session whose output keeps growing
        // shortly after its Stop is still working (codex mid-run Stops). The
        // ordinary busy idle-timeout then settles it once output stops.
        if entry.state == .idle, distrustStops, grew, let stoppedAt = entry.stoppedAt {
            let sinceStop = now.timeIntervalSince(stoppedAt)
            if sinceStop >= Self.stopRearmGrace, sinceStop <= Self.stopRearmWindow {
                entry.state = .busy
                entry.deadlineAt = now.addingTimeInterval(Self.hookIdleTimeout)
            }
        }
        return false
    }

    /// User-forced clear of a stuck attention badge ("Clear attention" in
    /// the sidebar context menu): drops an attention entry to idle. Busy and
    /// idle entries are untouched, and later hook events re-drive the state
    /// as usual.
    func clearAttention(_ sessionID: String) {
        guard var entry = entries[sessionID], entry.state == .attention else { return }
        entry.state = .idle
        entry.deadlineAt = nil
        entries[sessionID] = entry
    }

    func removeSession(_ sessionID: String) {
        entries.removeValue(forKey: sessionID)
    }

    func hasRuntimeOwnership(_ sessionID: String, generation: UInt64) -> Bool {
        entries[sessionID]?.ownedRuntimeGeneration == generation
    }

    /// Highest exact/effectively bound generation already accepted for this
    /// session. A new runtime's first hook can arrive before its manifest
    /// commit; once seen, a later old-generation hook must not overwrite it.
    func latestRuntimeGeneration(_ sessionID: String) -> UInt64? {
        guard let entry = entries[sessionID] else { return nil }
        return [entry.lastHookRuntimeGeneration, entry.ownedRuntimeGeneration]
            .compactMap { $0 }
            .max()
    }

    /// Reset hook ownership for a new managed-runtime generation without
    /// erasing a hook that the replacement agent already delivered. A nil
    /// result means no current-generation hook survived; a non-nil value says
    /// whether the preserved hook itself completed the new turn.
    @discardableResult
    func resetForRuntimeLaunch(
        _ sessionID: String,
        runtimeGeneration: UInt64,
        launchedAt: Date?
    ) -> Bool? {
        guard let entry = entries[sessionID] else { return nil }
        let exactGeneration = entry.lastHookRuntimeGeneration == runtimeGeneration
        let provenOwner = entry.ownedRuntimeGeneration == runtimeGeneration
        let legacyOwnerAfterLaunch: Bool = {
            guard let launchedAt, let startedAt = entry.legacyTurnStartedAt else {
                return false
            }
            return startedAt >= launchedAt
        }()
        let unversionedEventAfterLaunch: Bool = {
            guard entry.lastHookRuntimeGeneration == nil,
                  let launchedAt,
                  let lastHookEventAt = entry.lastHookEventAt
            else { return false }
            return lastHookEventAt >= launchedAt
        }()
        // A replacement's legacy Start can beat manifest commit, followed by
        // a delayed but explicitly tagged Stop from the old process. The tag
        // makes that latest event stale; restore the post-launch opener rather
        // than letting the stale event erase the replacement's busy state.
        if legacyOwnerAfterLaunch,
           let lastHookRuntimeGeneration = entry.lastHookRuntimeGeneration,
           lastHookRuntimeGeneration < runtimeGeneration,
           let startName = entry.legacyTurnStartEventName,
           let startedAt = entry.legacyTurnStartedAt {
            entries.removeValue(forKey: sessionID)
            applyHookEvent(
                sessionID: sessionID,
                hookEventName: startName,
                runtimeGeneration: runtimeGeneration,
                now: startedAt
            )
            return false
        }
        if exactGeneration
            || (unversionedEventAfterLaunch && (provenOwner || legacyOwnerAfterLaunch)) {
            // The fast hook was applied before Swift observed the manifest
            // generation, so `entry` may still carry state/output baselines
            // from the previous process. Re-apply only that new hook to a
            // fresh entry instead of cross-binding the old busy/attention.
            let completed = entry.lastHookCompletedTurn
            let preserveOwnership = provenOwner || legacyOwnerAfterLaunch
            entries.removeValue(forKey: sessionID)
            if let hookEventName = entry.lastHookEventName,
               let lastHookEventAt = entry.lastHookEventAt {
                applyHookEvent(
                    sessionID: sessionID,
                    hookEventName: hookEventName,
                    latchOnly: entry.lastHookWasLatchOnly,
                    runtimeGeneration: runtimeGeneration,
                    now: lastHookEventAt
                )
                if preserveOwnership, var rebound = entries[sessionID] {
                    rebound.ownedRuntimeGeneration = runtimeGeneration
                    entries[sessionID] = rebound
                }
            }
            return completed
        }
        entries.removeValue(forKey: sessionID)
        return nil
    }

    /// Drop entries for sessions that no longer exist (sweep parity with
    /// remove_session on missing manifests).
    func retainSessions(_ sessionIDs: Set<String>) {
        entries = entries.filter { sessionIDs.contains($0.key) }
    }
}

// MARK: - Durable hook-event seed (last-hook-event.json)

/// Parsed form of `last-hook-event.json`, the durable record every provider
/// hook script keeps of a session's most recent lifecycle event. Hook scripts
/// keep firing while no app instance is listening (the port POST just fails),
/// so this file survives an app restart and lets UnpeelStore re-seed the
/// in-memory hook latch — restoring busy/attention spinners for sessions that
/// were mid-turn when the app closed.
struct LastHookEvent {
    let hookEventName: String
    /// Mirrors handleHookEvent: AskUserQuestion permission events and unknown
    /// event names latch hook ownership without changing busy/idle state.
    let latchOnly: Bool
    let runtimeGeneration: UInt64?

    /// True when the recorded event opened a turn that never got a Stop —
    /// i.e. the agent was mid-turn when this file was last written. Turns
    /// routinely outlive the 5-minute hook idle timeout, so the seeding path
    /// must anchor these on output.bin recency, not the event's own mtime.
    var startsTurn: Bool {
        !latchOnly && (hookEventName == "Start" || hookEventName == "UserPromptSubmit")
    }

    func shouldAnchorSeedToOutput(anchorStartEventToOutput: Bool = true) -> Bool {
        startsTurn && (hookEventName != "Start" || anchorStartEventToOutput)
    }

    static func parse(_ data: Data) -> LastHookEvent? {
        guard let json = try? JSONSerialization.jsonObject(with: data) as? [String: Any],
              let rawName = json["hook_event_name"] as? String,
              !rawName.isEmpty
        else { return nil }
        let name = Self.normalizedHookEventName(rawName)
        let latchOnly: Bool
        switch name {
        case "Start", "UserPromptSubmit", "Stop", "StopFailure":
            latchOnly = false
        case "PermissionRequest":
            latchOnly = (json["tool_name"] as? String) == "AskUserQuestion"
        default:
            latchOnly = true
        }
        return LastHookEvent(
            hookEventName: name,
            latchOnly: latchOnly,
            runtimeGeneration: Self.runtimeGeneration(from: json)
        )
    }

    /// Provider hook scripts spell lifecycle events differently; the seed
    /// file keeps the raw name, so normalize exactly like the Rust Host does.
    static func normalizedHookEventName(_ raw: String) -> String {
        let trimmed = raw.trimmingCharacters(in: .whitespacesAndNewlines)
        let key = trimmed
            .replacingOccurrences(of: "-", with: "_")
            .replacingOccurrences(of: " ", with: "_")
            .lowercased()
        switch key {
        case "start":
            return "Start"
        // SessionStart means "the CLI opened / resumed a conversation", not
        // "the agent is working".
        case "session_start", "sessionstart":
            return "HookSeen"
        case "user_prompt_submit", "userpromptsubmit", "user_prompt_submitted",
             "before_submit_prompt", "beforesubmitprompt":
            return "UserPromptSubmit"
        case "stop", "session_end", "sessionend", "subagent_stop", "subagentstop":
            return "Stop"
        case "stop_failure", "stopfailure":
            return "StopFailure"
        case "permission_request", "permissionrequest":
            return "PermissionRequest"
        default:
            return trimmed
        }
    }

    static func runtimeGeneration(from json: [String: Any]) -> UInt64? {
        for key in ["unpeel_runtime_generation", "unpeelRuntimeGeneration"] {
            guard let number = json[key] as? NSNumber else { continue }
            guard CFGetTypeID(number) != CFBooleanGetTypeID(),
                  let generation = UInt64(number.stringValue)
            else { continue }
            return generation
        }
        return nil
    }
}

// MARK: - Unread reconciliation (sessionUnread.ts)

/// Pure port of reconcileUnreadSessions (sessionUnread.ts:59-100).
/// When the observed session changes away from a busy/attention session,
/// that session becomes "pending"; once it settles to idle/exited it is
/// marked unread if it exited or completed a hook turn while unobserved.
struct UnreadReconciliation {
    var pendingUnreadSessions: Set<String>
    var unreadToClear: [String] = []
    var unreadToMark: [String] = []

    static func reconcile(
        pendingUnreadSessions: Set<String>,
        sessionStates: [String: SessionStatus],
        completedSessionIDs: Set<String>,
        previousObservedSessionID: String?,
        currentObservedSessionID: String?
    ) -> UnreadReconciliation {
        var result = UnreadReconciliation(pendingUnreadSessions: pendingUnreadSessions)

        if let previous = previousObservedSessionID, previous != currentObservedSessionID {
            let previousState = sessionStates[previous]
            if previousState == .busy || previousState == .attention {
                result.pendingUnreadSessions.insert(previous)
            }
        }

        if let current = currentObservedSessionID {
            result.unreadToClear.append(current)
            result.pendingUnreadSessions.remove(current)
        }

        for sessionID in result.pendingUnreadSessions {
            guard let state = sessionStates[sessionID] else {
                // Session vanished entirely; nothing to surface.
                result.pendingUnreadSessions.remove(sessionID)
                continue
            }
            guard state == .idle || state == .exited else { continue }

            result.pendingUnreadSessions.remove(sessionID)
            let shouldMarkUnread = state == .exited || completedSessionIDs.contains(sessionID)
            if shouldMarkUnread, currentObservedSessionID != sessionID {
                result.unreadToMark.append(sessionID)
            }
        }

        return result
    }
}
