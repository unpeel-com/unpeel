import Foundation
import Testing
import UnpeelShared
@testable import UnpeelNative

struct PluginSettingsListTests {
    @Test func appQuickAccessUsesOneChipWithAllVariantsInHostOrder() {
        let app = RemoteAppSummary(id: "unpeel.app.markdown", name: "Markdown", command: "unpeel-markdown", installed: true)
        let presets = [
            Preset(id: "markdown", label: "Markdown", command: "unpeel-markdown", enabled: true, quickLaunch: true),
            Preset(id: "notes", label: "Notes", command: "unpeel-markdown notes.md", enabled: true, quickLaunch: false),
            Preset(id: "claude", label: "Claude", command: "claude", enabled: true, quickLaunch: true),
        ]
        let groups = collectQuickPresetGroups(presets, apps: [app])
        #expect(groups.map(\.id) == ["unpeel.app.markdown", "claude"])
        #expect(groups[0].presets.map(\.id) == ["markdown", "notes"])
        #expect(groups[0].displayName == "Markdown")
        #expect(collectQuickPresetGroups(presets, apps: []).map(\.id) == ["claude"])
    }

    @Test func mixedRowsKeepVariantsTogetherAndIgnoreLegacyProjectOverrides() throws {
        let settings = try JSONDecoder().decode(RemoteWorkspaceSettings.self, from: Data(#"{"pluginOrder":["unpeel.app.markdown","claude"],"availableAgents":[{"id":"claude","name":"Claude","command":"claude","installed":true}],"autoStopArchiveMinutes":120,"sidebarStoppedLimit":5,"browserDefaultAccess":"on","mcpNonchildWriteAccess":"ask","computerAccess":"ask","mcpWorktreeAccess":false,"mcpAutoAddBrowserScreenshots":true}"#.utf8))
        let snapshot = RemoteBootstrapSnapshot(
            macID: "remote", macName: "Remote", folders: [], projects: [],
            presets: [
                .init(id: "base", label: "Project override", command: "ignored", projectID: "project"),
                .init(id: "base", label: "Claude", command: "claude", pluginID: "claude"),
                .init(id: "variant", label: "Plan", command: "claude --plan", pluginID: "claude"),
                .init(id: "markdown", label: "Markdown", command: "unpeel-markdown", pluginID: "unpeel.app.markdown"),
            ],
            workspaceSettings: settings,
            availableApps: [.init(id: "unpeel.app.markdown", name: "Markdown", command: "unpeel-markdown", installed: true)],
            sessions: [], capturedAtUnixMs: 1
        )
        let items = PluginSettingsList.items(in: snapshot)
        #expect(items.map(\.id) == ["unpeel.app.markdown", "claude"])
        #expect(items[1].commands.map(\.command) == ["claude", "claude --plan"])
    }

    @Test func filteredDragPreservesHiddenAndInactiveSlots() {
        #expect(PluginSettingsList.merging(["codex", "claude"], into: ["claude", "hidden", "codex", "inactive"])
                == ["codex", "hidden", "claude", "inactive"])
    }

    @Test func draggingTallCardOpensItsFullHeightBetweenShortRows() {
        // A three-command card is 112pt high. Its 16pt gap moves along with it.
        let ids = ["claude", "codex", "markdown", "git"]
        #expect(PluginListDragController.reordered(ids, source: 0, target: 2) == ["codex", "markdown", "claude", "git"])
        #expect(PluginListDragController.slotOffset(index: 1, source: 0, target: 2, stride: 128) == -128)
        #expect(PluginListDragController.slotOffset(index: 2, source: 0, target: 2, stride: 128) == -128)
        #expect(PluginListDragController.slotOffset(index: 3, source: 0, target: 2, stride: 128) == 0)
        #expect(PluginListDragController.slotOffset(index: 1, source: 0, target: 0, stride: 128) == 0)
    }
}
