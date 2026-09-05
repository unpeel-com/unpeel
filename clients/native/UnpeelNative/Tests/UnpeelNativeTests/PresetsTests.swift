import XCTest
@testable import UnpeelNative
import UnpeelShared

final class PresetsTests: XCTestCase {
    func testSetupAndQuickToolsAreCatalogBacked() {
        XCTAssertEqual(
            SetupTool.allCases.map(\.rawValue),
            UnpeelRuntimeCatalog.runtimes(for: .macos).map(\.legacySlug)
        )
        XCTAssertEqual(
            QuickPresetTool.allCases.map(\.rawValue),
            UnpeelRuntimeCatalog.runtimes(for: .macos)
                .filter(\.supportsQuickLaunch)
                .map(\.legacySlug)
        )
        XCTAssertEqual(SetupTool.detect(in: "/usr/local/bin/claude --help"), .claude)
    }

    func testUsageStoresComeFromRuntimeMetadata() {
        XCTAssertEqual(ToolUsageScanner.stores(for: .claude).map(\.root), [".claude/projects"])
        XCTAssertEqual(
            ToolUsageScanner.stores(for: .kimi).map(\.root),
            [".kimi-code/sessions", ".kimi/sessions"]
        )
        XCTAssertTrue(ToolUsageScanner.stores(for: .amp).isEmpty)
    }

    func testLifecycleQuirksComeFromRuntimeMetadata() throws {
        let grok = try XCTUnwrap(SetupTool.grok.metadata)
        XCTAssertFalse(grok.anchorStartEventToOutput)
        XCTAssertFalse(grok.attentionClearsOnOutput)
        XCTAssertFalse(grok.distrustStopsWhileOutputGrows)

        let codex = try XCTUnwrap(SetupTool.codex.metadata)
        XCTAssertTrue(codex.anchorStartEventToOutput)
        XCTAssertTrue(codex.attentionClearsOnOutput)
        XCTAssertTrue(codex.distrustStopsWhileOutputGrows)

        let claude = try XCTUnwrap(SetupTool.claude.metadata)
        XCTAssertTrue(claude.anchorStartEventToOutput)
        XCTAssertTrue(claude.attentionClearsOnOutput)
        XCTAssertFalse(claude.distrustStopsWhileOutputGrows)
    }

    func testPersistedFavoriteSupportsEveryKnownCLI() throws {
        let raw = Data("""
        {
          "id": "grok",
          "label": "grok --always-approve",
          "command": "grok --always-approve",
          "project_id": null,
          "enabled": true,
          "quick_launch": true
        }
        """.utf8)

        let decoded = try JSONDecoder().decode(GlobalPresetFile.self, from: raw).runtime

        XCTAssertTrue(decoded.quickLaunch)
    }

    func testUnknownPersistedFavoriteIsSanitizedOff() throws {
        let raw = Data("""
        {
          "id": "custom",
          "label": "custom-agent",
          "command": "custom-agent",
          "project_id": null,
          "enabled": true,
          "quick_launch": true
        }
        """.utf8)

        let decoded = try JSONDecoder().decode(GlobalPresetFile.self, from: raw).runtime

        XCTAssertFalse(decoded.quickLaunch)
    }

    func testQuickPresetGroupsPreserveListOrderAcrossKnownCLIs() {
        let presets = [
            Preset(
                id: "grok",
                label: "grok --always-approve",
                command: "grok --always-approve",
                enabled: true,
                quickLaunch: true
            ),
            Preset(
                id: "cursor-agent",
                label: "cursor-agent",
                command: "cursor-agent",
                enabled: true,
                quickLaunch: true
            ),
            Preset(
                id: "kimi",
                label: "kimi --yolo",
                command: "kimi --yolo",
                enabled: true,
                quickLaunch: true
            ),
            Preset(
                id: "kiro-cli",
                label: "kiro-cli --v3",
                command: "kiro-cli --v3",
                enabled: true,
                quickLaunch: true
            ),
            Preset(
                id: "cline",
                label: "cline",
                command: "cline",
                enabled: true,
                quickLaunch: true
            ),
        ]

        // Groups follow the flat list order, one chip per CLI.
        XCTAssertEqual(
            collectQuickPresetGroups(presets).map(\.leader.id),
            ["grok", "cursor-agent", "kimi", "kiro-cli", "cline"]
        )
    }

    func testQuickPresetGroupsCollapseSameCLIStarsIntoOneGroup() {
        let presets = [
            Preset(
                id: "claude-plan",
                label: "claude --plan",
                command: "claude --plan",
                enabled: true,
                quickLaunch: true
            ),
            Preset(
                id: "grok",
                label: "grok --always-approve",
                command: "grok --always-approve",
                enabled: true,
                quickLaunch: true
            ),
            Preset(
                id: "claude",
                label: "claude",
                command: "claude",
                enabled: true,
                quickLaunch: true
            ),
            Preset(
                id: "claude-off",
                label: "claude --off",
                command: "claude --off",
                enabled: false,
                quickLaunch: true
            ),
            Preset(
                id: "codex-unstarred",
                label: "codex",
                command: "codex",
                enabled: true,
                quickLaunch: false
            ),
        ]

        let groups = collectQuickPresetGroups(presets)
        // One claude group (leader = topmost starred claude, disabled and
        // unstarred presets excluded), then grok; group order follows the
        // first starred preset of each CLI.
        XCTAssertEqual(groups.map(\.cli), [.claude, .grok])
        XCTAssertEqual(groups[0].presets.map(\.id), ["claude-plan", "claude"])
        XCTAssertEqual(groups[0].leader.id, "claude-plan")
    }

    // MARK: - Usage-ranked setup scan

    private func usage(_ total: Int, recent: Int, lastUsed: Date? = nil) -> ToolUsageStats {
        ToolUsageStats(sessionCount: total, recentCount: recent, lastUsed: lastUsed)
    }

    func testUsageOrderingPrefersRecentOverLifetimeVolume() {
        // Codex: huge lifetime, dormant. Claude: smaller but active.
        XCTAssertTrue(ToolUsageStats.moreUsed(usage(100, recent: 40), usage(900, recent: 2)))
        // Same recency: lifetime volume breaks the tie.
        XCTAssertTrue(ToolUsageStats.moreUsed(usage(900, recent: 5), usage(100, recent: 5)))
        // Fully equal stats are not strictly ordered either way.
        XCTAssertFalse(ToolUsageStats.moreUsed(usage(10, recent: 3), usage(10, recent: 3)))
        XCTAssertFalse(ToolUsageStats.moreUsed(.none, .none))
    }

    func testReportRanksInstalledToolsByUsageAndKeepsUnusedDeclarationOrder() {
        let report = ToolScanReport(statuses: [
            ToolInstallStatus(tool: .claude, path: "/bin/claude", usage: usage(200, recent: 10)),
            ToolInstallStatus(tool: .codex, path: "/bin/codex", usage: usage(900, recent: 30)),
            ToolInstallStatus(tool: .grok, path: "/bin/grok", usage: .none),
            ToolInstallStatus(tool: .gemini, path: "/bin/gemini", usage: .none),
            ToolInstallStatus(tool: .amp, path: nil, usage: usage(50, recent: 50)),
        ])

        // Not-installed tools are excluded; unused installed tools keep their
        // relative order at the end.
        XCTAssertEqual(report.usageOrderedInstalledTools, [.codex, .claude, .grok, .gemini])
        XCTAssertEqual(report.mostUsedTool, .codex)
    }

    func testMostUsedNeedsAFewSessionsToAvoidStrayFileBadges() {
        let report = ToolScanReport(statuses: [
            ToolInstallStatus(tool: .claude, path: "/bin/claude", usage: usage(2, recent: 2)),
            ToolInstallStatus(tool: .codex, path: "/bin/codex", usage: .none),
        ])
        XCTAssertNil(report.mostUsedTool)
    }

    func testUsageSummaryFormatsCountAndRecency() {
        XCTAssertNil(ToolUsageStats.none.summary)
        XCTAssertEqual(usage(1, recent: 0).summary, "1 session")
        XCTAssertEqual(
            usage(42, recent: 5, lastUsed: Date()).summary,
            "42 sessions · used today"
        )
        XCTAssertEqual(
            usage(42, recent: 5, lastUsed: Date().addingTimeInterval(-86_400)).summary,
            "42 sessions · used yesterday"
        )
        // Stale stores don't advertise a "used N days ago" beyond two months.
        XCTAssertEqual(
            usage(7, recent: 0, lastUsed: Date().addingTimeInterval(-86_400 * 120)).summary,
            "7 sessions"
        )
    }
}
