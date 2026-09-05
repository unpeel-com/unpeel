import Foundation
import UnpeelShared

/// The single source of truth for which session verbs a CLI supports —
/// Resume, Resume Agent, and Notify when done. The desktop
/// context menu reads it directly (`UnpeelStore.sessionCan*`); the phone's
/// session sheet gets the same answers as `RemoteSessionCapabilities` on
/// every session summary, so the phone never parses commands itself and an
/// old phone against a new Mac (or vice versa) degrades gracefully.
///
/// Declared provider support comes from `runtimes/*/runtime.toml`. Runtime
/// recipes and the final per-Session decision remain Host-owned.
enum ProviderCapabilities {
    /// First hosted-PTY protocol that implements shell-only, generation-bound
    /// Resume Agent.
    static let resumeAgentHostProtocolVersion = 3

    /// Static runtime support only. Effective Resume/Archive also requires
    /// `SessionEntry.hasResumableState`; recognizing a CLI or pre-assigning a
    /// provider id does not prove this particular launch created a session.
    static func canRestart(command: String) -> Bool {
        let trimmed = command.trimmingCharacters(in: .whitespacesAndNewlines)
        if trimmed.isEmpty { return false }
        return SetupTool.detect(in: trimmed)?.metadata?.capabilities.contains(.resume) == true
    }

    static func canArchive(session: SessionEntry) -> Bool {
        session.hasResumableState && canRestart(command: session.command)
    }

    /// Resume Agent applies only after a stable managed launch has exited to
    /// the shell in a still-live terminal. An active runtime — including the
    /// matching managed one — never exposes an action, and a passively
    /// observed agent in a blank terminal never acquires this capability.
    static func canResumeAgent(
        command: String,
        isLive: Bool,
        activeRuntimeID: String?,
        runtimeLaunchPending: Bool = false,
        hostProtocolVersion: Int?
    ) -> Bool {
        guard isLive,
              activeRuntimeID == nil,
              !runtimeLaunchPending,
              (hostProtocolVersion ?? 0) >= resumeAgentHostProtocolVersion,
              let launch = SetupTool.detect(in: command),
              launch.metadata?.capabilities.contains(.restartAgent) == true
        else { return false }
        return true
    }

    /// "Notify when done" fires on the hook Stop event; without lifecycle
    /// hooks (pi, plain shells, unknown commands) "done" is an
    /// output-settling guess, so the verb is not offered.
    static func canNotifyWhenDone(command: String) -> Bool {
        SetupTool.detect(in: command)?.metadata?.capabilities.contains(.notifyWhenDone) == true
    }

    /// Session-aware variant: an installed Unpeel App reports its own
    /// lifecycle (Stop included) through the hook port, so its "done" is
    /// hook-authoritative and the toggle is offered like any hook-capable
    /// agent's.
    static func canNotifyWhenDone(session: SessionEntry) -> Bool {
        session.activeApp != nil || canNotifyWhenDone(command: session.command)
    }

    /// The wire form shipped to paired phones on each session summary.
    static func remote(session: SessionEntry) -> RemoteSessionCapabilities {
        RemoteSessionCapabilities(
            // Legacy `session.restart` replaces the terminal and is now the
            // stopped-Session Resume operation. Live Sessions advertise the
            // separate in-place `resumeAgent` capability instead.
            restart: !session.isLive && canArchive(session: session),
            // Decode-only compatibility field. New Hosts never advertise the
            // old active-runtime restart affordance.
            restartAgent: nil,
            resumeAgent: session.hasResumableState
                && session.status != .starting && canResumeAgent(
                command: session.command,
                isLive: session.isLive,
                activeRuntimeID: session.activeRuntimeID,
                runtimeLaunchPending: session.runtimeLaunchPending,
                hostProtocolVersion: session.hostProtocolVersion
            ),
            notifyWhenDone: canNotifyWhenDone(session: session),
            // Archive is offered only for resumable commands — filing away
            // a session whose CLI can't resume just strands it in the
            // library, so non-resumable sessions offer Remove instead. (The
            // flag also hides the verb against older Macs whose organization
            // patch ignores `archived`.)
            archive: canArchive(session: session)
        )
    }
}
