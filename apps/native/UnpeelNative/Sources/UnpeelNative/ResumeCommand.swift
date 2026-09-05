import Foundation

/// Provider-neutral transport for Host-owned relaunch planning.
///
/// All command rewriting, provider identity selection, and verified
/// resume-failure markers live in unpeel-core runtime adapters. Native
/// deliberately fails closed when the bundled Host cannot produce a plan.
enum ResumeCommand {
    struct RelaunchPlan: Equatable {
        let command: String
        let failureMarkers: [String]
    }

    static func hostRelaunchPlan(
        sessionID: String,
        forceFresh: Bool = false
    ) -> RelaunchPlan? {
        let process = Process()
        process.executableURL = URL(fileURLWithPath: LaunchConfig.hostBinary)
        var arguments = ["__resume__", sessionID]
        if forceFresh {
            arguments.append("--fresh")
        }
        process.arguments = arguments
        process.standardInput = FileHandle.nullDevice
        let stdout = Pipe()
        process.standardOutput = stdout
        process.standardError = Pipe()
        do { try process.run() } catch { return nil }
        process.waitUntilExit()
        guard process.terminationStatus == 0,
              let data = try? stdout.fileHandleForReading.readToEnd()
        else { return nil }
        return decodeRelaunchPlan(data)
    }

    static func hostManagedStoragePath(sessionID: String) -> String? {
        let process = Process()
        process.executableURL = URL(fileURLWithPath: LaunchConfig.hostBinary)
        process.arguments = ["__managed_storage__", sessionID]
        process.standardInput = FileHandle.nullDevice
        let stdout = Pipe()
        process.standardOutput = stdout
        process.standardError = Pipe()
        do { try process.run() } catch { return nil }
        process.waitUntilExit()
        guard process.terminationStatus == 0,
              let data = try? stdout.fileHandleForReading.readToEnd()
        else { return nil }
        let path = String(decoding: data, as: UTF8.self)
            .trimmingCharacters(in: .whitespacesAndNewlines)
        return path.isEmpty ? nil : path
    }

    /// Kept separate from process execution so the additive Host response is
    /// covered without launching a binary in native unit tests. Older Hosts
    /// that return only `command` remain readable; missing markers mean the UI
    /// simply does not offer provider-specific failure recovery.
    static func decodeRelaunchPlan(_ data: Data) -> RelaunchPlan? {
        guard let object = (try? JSONSerialization.jsonObject(with: data)) as? [String: Any],
              let command = object["command"] as? String,
              !command.isEmpty
        else { return nil }
        let markers = (object["failure_markers"] as? [String])?
            .filter { !$0.isEmpty } ?? []
        return RelaunchPlan(command: command, failureMarkers: markers)
    }
}
