import XCTest
@testable import UnpeelNative
import UnpeelShared

final class ProviderCapabilitiesTests: XCTestCase {
    func testDeclaredGatesMirrorGeneratedRuntimeCapabilities() throws {
        for runtime in UnpeelRuntimeCatalog.runtimes(for: .macos) {
            let command = try XCTUnwrap(runtime.commandAliases.first)
            XCTAssertEqual(
                ProviderCapabilities.canRestart(command: command),
                runtime.capabilities.contains(.resume),
                runtime.slug
            )
            XCTAssertEqual(
                ProviderCapabilities.canNotifyWhenDone(command: command),
                runtime.capabilities.contains(.notifyWhenDone),
                runtime.slug
            )
        }
    }

    func testRestartStaticGateRequiresAKnownAgentRecipe() {
        // Every CLI ResumeCommand can resume has static runtime support.
        for command in [
            "claude --dangerously-skip-permissions", "cline", "codex", "amp",
            "gemini --yolo", "pi", "opencode", "cursor-agent",
            "grok --always-approve", "kimi --yolo", "kiro-cli --v3", "muse --yolo", "copilot",
        ] {
            XCTAssertTrue(ProviderCapabilities.canRestart(command: command), command)
        }
        // A blank terminal has no provider conversation to resume.
        XCTAssertFalse(ProviderCapabilities.canRestart(command: ""))
        // An unknown command would silently restart as a fresh conversation.
        XCTAssertFalse(ProviderCapabilities.canRestart(command: "my-custom-agent --serve"))
    }

    func testResumeAgentRequiresReturnedShellAndHostProtocolThree() {
        XCTAssertTrue(ProviderCapabilities.canResumeAgent(
            command: "claude", isLive: true, activeRuntimeID: nil,
            hostProtocolVersion: 3
        ))
        // An active runtime never exposes an action, even when it matches the
        // stable managed launch.
        XCTAssertFalse(ProviderCapabilities.canResumeAgent(
            command: "claude", isLive: true, activeRuntimeID: "claude",
            hostProtocolVersion: 3
        ))
        // A blank terminal remains presentation-only.
        XCTAssertFalse(ProviderCapabilities.canResumeAgent(
            command: "", isLive: true, activeRuntimeID: "claude",
            hostProtocolVersion: 3
        ))
        XCTAssertFalse(ProviderCapabilities.canResumeAgent(
            command: "claude", isLive: true, activeRuntimeID: "codex",
            hostProtocolVersion: 3
        ))
        XCTAssertFalse(ProviderCapabilities.canResumeAgent(
            command: "claude", isLive: false, activeRuntimeID: nil,
            hostProtocolVersion: 3
        ))
        XCTAssertFalse(ProviderCapabilities.canResumeAgent(
            command: "claude", isLive: true, activeRuntimeID: nil,
            runtimeLaunchPending: true,
            hostProtocolVersion: 3
        ))
        XCTAssertFalse(ProviderCapabilities.canResumeAgent(
            command: "claude", isLive: true, activeRuntimeID: nil,
            hostProtocolVersion: 2
        ))
    }

    func testSidebarResumePresentationMatchesRuntimeAndArchiveState() {
        func presentation(
            command: String = "claude",
            status: SessionStatus = .idle,
            activeRuntimeID: String? = nil,
            hostProtocolVersion: Int = 3,
            archived: Bool = false
        ) -> SessionRowResumePresentation {
            let session = SessionEntry(
                id: "session", projectID: "project", label: "Session",
                command: command, createdAt: 1, status: status,
                activeRuntimeID: activeRuntimeID,
                hostProtocolVersion: hostProtocolVersion
            )
            return sessionRowResumePresentation(
                session: session,
                isArchived: archived,
                canRestart: ProviderCapabilities.canRestart(command: command),
                canResumeAgent: ProviderCapabilities.canResumeAgent(
                    command: command,
                    isLive: session.isLive,
                    activeRuntimeID: activeRuntimeID,
                    hostProtocolVersion: hostProtocolVersion
                )
            )
        }

        XCTAssertEqual(
            presentation(activeRuntimeID: "claude"),
            .none,
            "an active runtime must not expose a destructive agent action"
        )
        XCTAssertEqual(presentation(), .resumeAgent)
        XCTAssertEqual(presentation(hostProtocolVersion: 2), .none)
        XCTAssertEqual(
            presentation(command: "custom-tool", status: .exited, archived: true),
            .restore
        )
        XCTAssertEqual(
            presentation(status: .exited, archived: true),
            .restoreAndResume
        )

        XCTAssertTrue(sessionRowShowsInlineResume(.resumeAgent))
        XCTAssertTrue(sessionRowShowsInlineResume(.resumeSession))
        XCTAssertTrue(sessionRowShowsInlineResume(.restoreAndResume))
        XCTAssertFalse(sessionRowShowsInlineResume(.none))
        XCTAssertFalse(sessionRowShowsInlineResume(.restore))
    }

    func testNotifyWhenDoneRequiresLifecycleHooks() {
        for command in [
            "claude", "cline", "codex", "amp", "gemini", "opencode",
            "cursor-agent", "grok", "kimi", "kiro-cli --v3", "muse --yolo", "copilot",
        ] {
            XCTAssertTrue(ProviderCapabilities.canNotifyWhenDone(command: command), command)
        }
        // pi has no hooks; shells and unknown commands settle by output guess.
        XCTAssertFalse(ProviderCapabilities.canNotifyWhenDone(command: "pi"))
        XCTAssertFalse(ProviderCapabilities.canNotifyWhenDone(command: ""))
        XCTAssertFalse(ProviderCapabilities.canNotifyWhenDone(command: "htop"))
    }

    func testTranscriptCopyRequiresAKnownTranscriptBackedAgentLaunch() {
        XCTAssertTrue(session(command: "claude").supportsTranscriptCopy)
        XCTAssertTrue(
            session(command: "/opt/homebrew/bin/codex --yolo")
                .supportsTranscriptCopy
        )

        XCTAssertFalse(session(command: "").supportsTranscriptCopy)
        XCTAssertFalse(session(command: "my-custom-agent").supportsTranscriptCopy)
        XCTAssertFalse(
            session(command: "amp").supportsTranscriptCopy,
            "a known agent without a transcript adapter must stay hidden"
        )
        XCTAssertFalse(
            session(command: "", activeRuntimeID: "claude")
                .supportsTranscriptCopy,
            "passive process observation must not promote a blank shell"
        )
    }

    func testCollapsedPaneRowMovesTranscriptCopyToEachPaneMenu() {
        let transcriptSession = session(command: "claude")
        XCTAssertTrue(sessionRowShowsCopyTranscript(
            session: transcriptSession,
            paneItems: []
        ))

        let pane = UnpeelStore.PaneSidebarItem(
            paneID: "pane",
            sessionID: transcriptSession.id,
            command: transcriptSession.command,
            agentName: "Claude",
            status: .idle,
            isRepresentative: true
        )
        XCTAssertFalse(sessionRowShowsCopyTranscript(
            session: transcriptSession,
            paneItems: [pane]
        ))
    }

    func testRemoteFormMirrorsTheGates() {
        let pi = ProviderCapabilities.remote(session: SessionEntry(
            id: "pi", projectID: "p", label: "Pi", command: "pi",
            createdAt: 1, status: .idle, activeRuntimeID: nil,
            hostProtocolVersion: 3, hasResumableState: true
        ))
        XCTAssertFalse(pi.restart)
        XCTAssertNil(pi.restartAgent)
        XCTAssertEqual(pi.resumeAgent, true)
        XCTAssertFalse(pi.notifyWhenDone)

        let unknown = ProviderCapabilities.remote(session: SessionEntry(
            id: "unknown", projectID: "p", label: "Unknown",
            command: "my-custom-agent", createdAt: 1, status: .idle,
            activeRuntimeID: nil, hostProtocolVersion: 3
        ))
        XCTAssertFalse(unknown.restart)
        XCTAssertNil(unknown.restartAgent)
        XCTAssertEqual(unknown.resumeAgent, false)
        XCTAssertFalse(unknown.notifyWhenDone)

        let stoppedBlank = ProviderCapabilities.remote(session: SessionEntry(
            id: "blank", projectID: "p", label: "Terminal", command: "",
            createdAt: 1, status: .exited, hostProtocolVersion: 3
        ))
        XCTAssertFalse(stoppedBlank.restart)
        XCTAssertEqual(stoppedBlank.resumeAgent, false)

        let stoppedUnknown = ProviderCapabilities.remote(session: SessionEntry(
            id: "unknown-stopped", projectID: "p", label: "Unknown",
            command: "my-custom-agent", createdAt: 1, status: .exited,
            hostProtocolVersion: 3
        ))
        XCTAssertFalse(stoppedUnknown.restart)
        XCTAssertEqual(stoppedUnknown.resumeAgent, false)

        let starting = ProviderCapabilities.remote(session: SessionEntry(
            id: "starting", projectID: "p", label: "Claude", command: "claude",
            createdAt: 1, status: .starting, activeRuntimeID: nil,
            hostProtocolVersion: 3
        ))
        XCTAssertFalse(starting.restart)
        XCTAssertEqual(starting.resumeAgent, false)

        let active = ProviderCapabilities.remote(session: SessionEntry(
            id: "active", projectID: "p", label: "Claude", command: "claude",
            createdAt: 1, status: .busy, activeRuntimeID: "claude",
            hostProtocolVersion: 3
        ))
        XCTAssertNil(active.restartAgent)
        XCTAssertEqual(active.resumeAgent, false)
    }

    private func session(
        command: String,
        activeRuntimeID: String? = nil
    ) -> SessionEntry {
        SessionEntry(
            id: "session",
            projectID: "project",
            label: "Session",
            command: command,
            createdAt: 1,
            status: .idle,
            activeRuntimeID: activeRuntimeID
        )
    }
}
