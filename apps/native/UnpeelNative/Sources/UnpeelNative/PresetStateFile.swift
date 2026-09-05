//
//  PresetStateFile.swift
//  UnpeelNative
//
//  Preset writes to the shared ~/.unpeel/app-state.json contract. Since the
//  overlay migration (2026-08-08) the file's `presets` array — its order
//  included — is the single source of truth for the flat preset list, shared
//  with the terminal UI (`unpeel`), which edits the same file. Edits happen
//  at the raw-JSON level so keys this build does not model — top-level or
//  per-preset — survive a rewrite, matching the guarded Rust editor
//  (unpeel-core `app_state::edit`).
//

import Foundation

enum PresetStateFile {
    /// Top-level app-state.json marker set by the one-time overlay fold.
    /// Once present, both UIs treat the file as the whole preset truth and
    /// ignore the legacy `unpeel.native.presets`/`presetOrder` defaults.
    static let migratedKey = "native_preset_overlay_migrated"

    /// Read-modify-write app-state.json. Refuses to touch a file it could
    /// not parse (a corrupt file must never be clobbered); creates the
    /// minimal skeleton when the file does not exist yet. Atomic via
    /// temp-file + rename, like the Rust editor.
    /// The same advisory lock the Rust editor takes (`<file>.lock`, flock):
    /// atomic rename prevents torn files, this prevents LOST UPDATES when
    /// the app and the terminal edit concurrently. Cross-process by
    /// construction — both languages lock the identical path.
    static func withExclusiveLock<T>(on url: URL, _ body: () -> T) -> T? {
        let lockURL = url.deletingPathExtension().appendingPathExtension("lock")
        let fd = open(lockURL.path, O_CREAT | O_WRONLY, 0o644)
        guard fd >= 0 else { return nil }
        defer { close(fd) }
        guard flock(fd, LOCK_EX) == 0 else { return nil }
        defer { flock(fd, LOCK_UN) }
        return body()
    }

    /// Edit this instance's own `~/.unpeel/app-state.json`.
    @discardableResult
    static func edit(_ mutate: (inout [String: Any]) -> Void) -> Bool {
        edit(at: LaunchConfig.appStateFile, mutate)
    }

    /// Edit an explicit `app-state.json` — used when a `.localWorkspace` scope
    /// runs a local-against-home state verb against the SCOPED workspace's own
    /// home rather than this instance's. Uses the identical `<file>.lock`
    /// advisory lock + atomic rename discipline, so the scoped workspace's own
    /// running app instance, the loopback gateway, and the TUI never lose a
    /// concurrent write.
    @discardableResult
    static func edit(at url: URL, _ mutate: (inout [String: Any]) -> Void) -> Bool {
        return withExclusiveLock(on: url) { lockedEdit(url: url, mutate) } ?? false
    }

    private static func lockedEdit(
        url: URL, _ mutate: (inout [String: Any]) -> Void
    ) -> Bool {
        var object: [String: Any]
        if let data = try? Data(contentsOf: url) {
            guard let parsed = (try? JSONSerialization.jsonObject(with: data)) as? [String: Any]
            else { return false }
            object = parsed
        } else {
            object = [
                "projects": [],
                "active_project_id": NSNull(),
                "presets": [],
                "active_tabs": [:],
                "pinned_sessions": [:],
            ]
        }
        mutate(&object)
        guard JSONSerialization.isValidJSONObject(object),
              let body = try? JSONSerialization.data(
                withJSONObject: object, options: [.prettyPrinted, .sortedKeys]
              )
        else { return false }
        let tmp = url.deletingLastPathComponent()
            .appendingPathComponent("app-state.json.native-tmp")
        do {
            try FileManager.default.createDirectory(
                at: url.deletingLastPathComponent(), withIntermediateDirectories: true
            )
            try body.write(to: tmp)
            _ = try FileManager.default.replaceItemAt(url, withItemAt: tmp)
            return true
        } catch {
            try? FileManager.default.removeItem(at: tmp)
            NSLog("[UnpeelNative] preset state write failed: \(error)")
            return false
        }
    }

    /// Collapse rows that are exact duplicates of an earlier row — same
    /// label, command, and project — keeping the first occurrence and its
    /// slot. A star on any dropped copy carries over so a user never loses a
    /// favorite. Rows that differ in label or command are never touched:
    /// this is the one-time repair for 0.4.0's client-mode Add bug, which
    /// appended a copy per click, not a deduplication policy.
    static func collapseExactDuplicates(
        _ rows: [[String: Any]]
    ) -> (rows: [[String: Any]], removed: Int) {
        var seen: [String: Int] = [:]
        var kept: [[String: Any]] = []
        var removed = 0
        for row in rows {
            let label = (row["label"] as? String ?? "").trimmingCharacters(in: .whitespaces)
            let command = (row["command"] as? String ?? "").trimmingCharacters(in: .whitespaces)
            let project = row["project_id"] as? String ?? ""
            let key = "\(label)\u{0}\(command)\u{0}\(project)"
            if let index = seen[key], !command.isEmpty {
                if row["quick_launch"] as? Bool == true {
                    kept[index]["quick_launch"] = true
                }
                removed += 1
                continue
            }
            seen[key] = kept.count
            kept.append(row)
        }
        return (kept, removed)
    }

    /// The file's raw preset dicts (empty when absent/unreadable).
    static func rawPresets(of object: [String: Any]) -> [[String: Any]] {
        ((object["presets"] as? [Any]) ?? []).compactMap { $0 as? [String: Any] }
    }

    /// Write `preset`'s modelled fields into a raw dict, keeping any keys
    /// this build does not model (`project_id`, Tauri-era extras).
    static func apply(_ preset: Preset, to dict: [String: Any]) -> [String: Any] {
        var dict = dict
        dict["id"] = preset.id
        dict["label"] = preset.label
        dict["command"] = preset.command
        dict["enabled"] = preset.enabled
        dict["quick_launch"] = preset.quickLaunch
        return dict
    }
}
