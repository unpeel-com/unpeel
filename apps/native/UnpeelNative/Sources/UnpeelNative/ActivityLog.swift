//
//  ActivityLog.swift
//  UnpeelNative
//
//  Persisted session-activity history: the data source for the "Recent"
//  panel and the always-visible titlebar bell. `activity-state.json` and
//  `last-hook-event.json` are current-state snapshots only, so this is the
//  one place that records *events over time* — session started, needed
//  input, finished, exited, or raised an App alert — durable across app
//  restarts.
//

import Foundation

/// One entry in the persisted history feed. Title/project/command are
/// snapshotted at log time so an entry stays renderable after its session
/// (or even the project) is removed.
struct ActivityLogEntry: Codable, Identifiable, Equatable {
    enum Kind: String, Codable {
        case started
        case needsInput = "needs_input"
        case finished
        case exited
        /// App-owned informational alert; never a lifecycle transition.
        case alert
    }

    let id: String
    let sessionID: String
    let kind: Kind
    /// ms epoch
    let at: UInt64
    let title: String
    /// Launch command, for the CLI/provider icon.
    let command: String
    let projectID: String
    let projectName: String
    /// App-provided alert copy. Nil for lifecycle entries and legacy logs.
    let message: String?

    init(
        id: String,
        sessionID: String,
        kind: Kind,
        at: UInt64,
        title: String,
        command: String,
        projectID: String,
        projectName: String,
        message: String? = nil
    ) {
        self.id = id
        self.sessionID = sessionID
        self.kind = kind
        self.at = at
        self.title = title
        self.command = command
        self.projectID = projectID
        self.projectName = projectName
        self.message = message
    }

    enum CodingKeys: String, CodingKey {
        case id
        case sessionID = "session_id"
        case kind
        case at
        case title
        case command
        case projectID = "project_id"
        case projectName = "project_name"
        case message
    }

    var date: Date { Date(timeIntervalSince1970: TimeInterval(at) / 1000) }
}

/// Append-only JSONL log at `<UNPEEL_HOME>/activity-log.jsonl` (same
/// per-home scoping as activity-state.json, so workspaces keep separate
/// feeds). Undecodable lines are skipped on load so the format can grow;
/// the file is compacted back to the in-memory tail whenever its line
/// count reaches double the entry cap.
@MainActor
final class ActivityLogStore {
    static let maxEntries = 300

    private struct FileStamp: Equatable {
        let size: UInt64
        let modifiedAt: Date?
    }

    private(set) var entries: [ActivityLogEntry] = []
    private let fileURL: URL
    private var fileLineCount = 0
    private var loadedStamp: FileStamp?

    init(fileURL: URL = LaunchConfig.activityLogFile) {
        self.fileURL = fileURL
        load()
    }

    /// Refresh the read model after the canonical Host appends lifecycle
    /// events. Client-only frontends never compact or rewrite the Host-owned
    /// file; the metadata stamp keeps the common unchanged snapshot cheap.
    @discardableResult
    func refreshFromHost() -> Bool {
        let stamp = currentStamp()
        guard stamp != loadedStamp else { return false }
        let previous = entries
        load(compactIfNeeded: false)
        return entries != previous
    }

    /// Append, collapsing a same-kind repeat for the same session into one
    /// bumped entry (a TUI re-requesting permission, or back-to-back turn
    /// finishes, refresh a single feed row instead of stacking).
    func append(_ entry: ActivityLogEntry) {
        Self.appendCollapsing(entry, to: &entries)
        if entries.count > Self.maxEntries {
            entries.removeFirst(entries.count - Self.maxEntries)
        }
        guard let line = try? JSONEncoder().encode(entry) else { return }
        do {
            let fm = FileManager.default
            try fm.createDirectory(
                at: fileURL.deletingLastPathComponent(),
                withIntermediateDirectories: true
            )
            if !fm.fileExists(atPath: fileURL.path) {
                fm.createFile(atPath: fileURL.path, contents: nil)
            }
            let handle = try FileHandle(forWritingTo: fileURL)
            defer { try? handle.close() }
            try handle.seekToEnd()
            try handle.write(contentsOf: line + Data("\n".utf8))
            fileLineCount += 1
            if fileLineCount >= Self.maxEntries * 2 { compact() }
            loadedStamp = currentStamp()
        } catch {
            NSLog("[UnpeelNative] failed to append activity-log: \(error)")
        }
    }

    static func appendCollapsing(
        _ entry: ActivityLogEntry, to entries: inout [ActivityLogEntry]
    ) {
        if let last = entries.lastIndex(where: { $0.sessionID == entry.sessionID }),
           entries[last].kind == entry.kind,
           entry.kind != .alert || entries[last].message == entry.message {
            entries.remove(at: last)
        }
        entries.append(entry)
    }

    private func load(compactIfNeeded: Bool = true) {
        guard let data = try? Data(contentsOf: fileURL),
              let text = String(data: data, encoding: .utf8)
        else {
            entries = []
            fileLineCount = 0
            loadedStamp = nil
            return
        }
        let decoder = JSONDecoder()
        let lines = text.split(separator: "\n", omittingEmptySubsequences: true)
        fileLineCount = lines.count
        var loaded: [ActivityLogEntry] = []
        for line in lines {
            guard let entry = try? decoder.decode(
                ActivityLogEntry.self, from: Data(line.utf8)
            ) else { continue }
            // Same collapse rule as append, so a reload renders the same
            // feed the previous run showed.
            Self.appendCollapsing(entry, to: &loaded)
        }
        entries = Array(loaded.suffix(Self.maxEntries))
        loadedStamp = currentStamp()
        if compactIfNeeded, fileLineCount >= Self.maxEntries * 2 { compact() }
    }

    /// Rewrite the file down to the in-memory tail.
    private func compact() {
        let encoder = JSONEncoder()
        var out = Data()
        for entry in entries {
            guard let line = try? encoder.encode(entry) else { continue }
            out.append(line)
            out.append(0x0A)
        }
        do {
            try out.write(to: fileURL, options: .atomic)
            fileLineCount = entries.count
            loadedStamp = currentStamp()
        } catch {
            NSLog("[UnpeelNative] failed to compact activity-log: \(error)")
        }
    }

    private func currentStamp() -> FileStamp? {
        guard let attributes = try? FileManager.default.attributesOfItem(
            atPath: fileURL.path
        ) else { return nil }
        let size = (attributes[.size] as? NSNumber)?.uint64Value ?? 0
        return FileStamp(
            size: size,
            modifiedAt: attributes[.modificationDate] as? Date
        )
    }
}
