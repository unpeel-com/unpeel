import Foundation
import UnpeelShared

/// Value projection shared by the live list and the frozen drag card.
struct PluginSettingsItem: Identifiable, Equatable {
    let id: String
    let name: String
    let command: String
    let appID: String?
    let installed: Bool
    let installCommand: String?
    let websiteURL: String?
    let installedVersion: String?
    let isApp: Bool
    let isCustom: Bool
    var commands: [RemotePresetSummary]
}

enum PluginSettingsList {
    static func items(in snapshot: RemoteBootstrapSnapshot?) -> [PluginSettingsItem] {
        guard let snapshot else { return [] }
        var items = (snapshot.workspaceSettings?.availableAgents ?? []).map {
            PluginSettingsItem(id: $0.id, name: $0.name, command: $0.command, appID: nil,
                               installed: $0.installed, installCommand: $0.installCommand,
                               websiteURL: $0.websiteURL, installedVersion: nil,
                               isApp: false, isCustom: false, commands: [])
        }
        items += (snapshot.availableApps ?? []).map {
            PluginSettingsItem(id: $0.id, name: $0.name, command: $0.command, appID: $0.id,
                               installed: $0.installed, installCommand: $0.installCommand,
                               websiteURL: nil, installedVersion: $0.installedVersion,
                               isApp: true, isCustom: false, commands: [])
        }
        for preset in snapshot.presets where preset.projectID == nil {
            let executable = (preset.command.split(separator: " ").first.map(String.init) ?? "")
                .trimmingCharacters(in: CharacterSet(charactersIn: "'\""))
            let fallback = items.first { $0.command == URL(fileURLWithPath: executable).lastPathComponent }?.id
            let id = preset.pluginID ?? fallback ?? "preset:\(preset.id)"
            if let index = items.firstIndex(where: { $0.id == id }) {
                items[index].commands.append(preset)
            } else {
                items.append(PluginSettingsItem(id: id, name: preset.label, command: preset.command,
                                                appID: nil, installed: true, installCommand: nil,
                                                websiteURL: nil, installedVersion: nil,
                                                isApp: false, isCustom: true, commands: [preset]))
            }
        }
        // Until the first explicit row move, retain the user's existing launch
        // order, then append catalog entries with no configured commands.
        let presetOrder = snapshot.presets.filter { $0.projectID == nil }.compactMap { preset in
            items.first { $0.commands.contains(where: { $0.id == preset.id }) }?.id
        }
        let order = (snapshot.workspaceSettings?.pluginOrder ?? []) + presetOrder
        let rank = Dictionary(order.enumerated().map { ($0.element, $0.offset) }, uniquingKeysWith: min)
        return items.enumerated().sorted {
            let left = rank[$0.element.id] ?? Int.max
            let right = rank[$1.element.id] ?? Int.max
            return left == right ? $0.offset < $1.offset : left < right
        }.map(\.element)
    }

    /// Reorder only the visible subset, leaving filtered and inactive rows in
    /// their original slots. The Host applies the same merge under its lock.
    static func merging(_ subset: [String], into order: [String]) -> [String] {
        var result = order
        for id in subset where !result.contains(id) { result.append(id) }
        let moved = Set(subset)
        var replacements = subset.makeIterator()
        return result.map { moved.contains($0) ? (replacements.next() ?? $0) : $0 }
    }
}
