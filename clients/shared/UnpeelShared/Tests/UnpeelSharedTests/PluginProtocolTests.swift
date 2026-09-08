import Foundation
import Testing
@testable import UnpeelShared

struct PluginProtocolTests {
    @Test func updateChecksPreserveUnknownAndPendingStates() throws {
        let result = try JSONDecoder().decode(RemotePluginUpdates.self, from: Data(#"{"checking":true,"items":[{"id":"missing-version","state":"unknown","updateAvailable":false},{"id":"pending","state":"checking","updateAvailable":false},{"id":"upgrade","state":"available","installedVersion":"1.0.0","latestVersion":"1.1.0","updateAvailable":true}]}"#.utf8))
        #expect(result.checking)
        #expect(result.items.filter(\.updateAvailable).map(\.id) == ["upgrade"])
        #expect(result.items.first?.installedVersion == nil)
    }

    private let legacySettings = Data(#"{"autoStopArchiveMinutes":120,"sidebarStoppedLimit":5,"browserDefaultAccess":"on","mcpNonchildWriteAccess":"ask","computerAccess":"ask","mcpWorktreeAccess":false,"mcpAutoAddBrowserScreenshots":true}"#.utf8)

    @Test func pluginOrderPatchAndCommandIdentityRoundTrip() throws {
        let patch = RemoteWorkspaceSettingsPatch(pluginOrder: ["codex", "unpeel.app.markdown", "claude"])
        #expect(!patch.isEmpty)
        let object = try #require(JSONSerialization.jsonObject(with: JSONEncoder().encode(patch)) as? [String: Any])
        #expect(object.count == 1)
        #expect(object["pluginOrder"] as? [String] == patch.pluginOrder)
        let preset = RemotePresetSummary(id: "variant", label: "Plan", command: "claude --plan", pluginID: "claude")
        #expect(try JSONDecoder().decode(RemotePresetSummary.self, from: JSONEncoder().encode(preset)) == preset)
    }

    @Test func appInstallerKeepsTheRemoteHostsAbsoluteCommand() throws {
        let app = RemoteAppSummary(id: "unpeel.app.markdown", name: "Markdown", command: "unpeel-markdown",
                                   installCommand: "'/opt/unpeel/bin/unpeel' apps install 'unpeel.app.markdown' --yes")
        let decoded = try JSONDecoder().decode(RemoteAppSummary.self, from: JSONEncoder().encode(app))
        #expect(decoded.installCommand == app.installCommand)
    }

    @Test func legacyPresetScopeSurvivesRoundTripAndMissingScopeStaysGlobal() throws {
        let scoped = RemotePresetSummary(id: "shared", label: "Project", command: "claude --plan", projectID: "project")
        let global = RemotePresetSummary(id: "shared", label: "Global", command: "claude")
        let data = try JSONEncoder().encode([scoped, global])
        #expect(try JSONDecoder().decode([RemotePresetSummary].self, from: data) == [scoped, global])
    }

    @Test func olderHostsStillDecodeWithoutPluginInventory() throws {
        let settings = try JSONDecoder().decode(RemoteWorkspaceSettings.self, from: legacySettings)
        #expect(settings.pluginActivation == nil)
        #expect(settings.availableAgents == nil)
        #expect(settings.pluginOrder == nil)
    }

    @Test func activationPatchChangesOnePluginWithoutReplacingOtherSettings() throws {
        let patch = RemoteWorkspaceSettingsPatch(pluginActivation: .init(id: "unpeel.app.markdown", active: false))
        #expect(!patch.isEmpty)
        let object = try #require(JSONSerialization.jsonObject(with: JSONEncoder().encode(patch)) as? [String: Any])
        #expect(object.count == 1)
        let activation = try #require(object["pluginActivation"] as? [String: Any])
        #expect(activation["id"] as? String == "unpeel.app.markdown")
        #expect(activation["active"] as? Bool == false)
    }

    @Test func hostInventoryAndActivationSurviveSnapshotRoundTrip() throws {
        var object = try #require(JSONSerialization.jsonObject(with: legacySettings) as? [String: Any])
        object["pluginActivation"] = ["com.openai.codex": false]
        object["availableAgents"] = [["id": "com.openai.codex", "name": "Codex", "command": "codex", "installed": true]]
        let settings = try JSONDecoder().decode(RemoteWorkspaceSettings.self, from: JSONSerialization.data(withJSONObject: object))
        #expect(settings.availableAgents?.first?.installed == true)
        #expect(settings.availableAgents?.first?.installCommand == nil)
        #expect(settings.pluginActivation?["com.openai.codex"] == false)
        #expect(try JSONDecoder().decode(RemoteWorkspaceSettings.self, from: JSONEncoder().encode(settings)) == settings)
    }
}
