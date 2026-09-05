//
//  TerminalDropTargetMap.swift
//  UnpeelNative
//
//  Hosted Ratatui Apps can publish short-lived terminal-cell rectangles that
//  accept semantic file/folder drops. The native Ghostty destination writes
//  hover and drop events back into that Session directory, allowing editors
//  to move their own caret and scroll while an AppKit drag is in flight.
//

import Foundation

struct TerminalDropTargetMap: Decodable, Equatable {
    struct Region: Decodable, Equatable {
        let screenRow: Int
        let startColumn: Int
        let endRow: Int
        let endColumn: Int

        enum CodingKeys: String, CodingKey {
            case screenRow = "screen_row"
            case startColumn = "start_column"
            case endRow = "end_row"
            case endColumn = "end_column"
        }

        func contains(row: Int, column: Int) -> Bool {
            row >= screenRow && row < endRow
                && column >= startColumn && column < endColumn
        }
    }

    let version: Int
    let processID: Int32
    let updatedAt: UInt64
    let regions: [Region]

    enum CodingKeys: String, CodingKey {
        case version
        case processID = "pid"
        case updatedAt = "updated_at"
        case regions
    }

    static let filename = "terminal-drop-target-map.json"
    static let eventFilename = "terminal-drop-target-event.json"
    static let maximumBytes: UInt64 = 64 * 1024
    static let maximumAgeMilliseconds: UInt64 = 5_000
    static let maximumFutureSkewMilliseconds: UInt64 = 5_000

    static func load(from sessionDirectory: URL) -> TerminalDropTargetMap? {
        let url = sessionDirectory.appendingPathComponent(filename)
        guard let attributes = try? FileManager.default.attributesOfItem(atPath: url.path),
              let byteCount = (attributes[.size] as? NSNumber)?.uint64Value,
              byteCount <= maximumBytes,
              let data = try? Data(contentsOf: url, options: [.mappedIfSafe])
        else { return nil }
        return try? JSONDecoder().decode(TerminalDropTargetMap.self, from: data)
    }

    func accepts(row: Int, column: Int, nowMilliseconds: UInt64) -> Bool {
        guard version == 1,
              processID > 0,
              updatedAt <= nowMilliseconds &+ Self.maximumFutureSkewMilliseconds,
              nowMilliseconds <= updatedAt &+ Self.maximumAgeMilliseconds
        else { return false }
        return regions.contains { $0.contains(row: row, column: column) }
    }

    static func writeEvent(
        kind: TerminalDropTargetEvent.Kind,
        row: Int? = nil,
        column: Int? = nil,
        text: String? = nil,
        references: [String]? = nil,
        to sessionDirectory: URL
    ) -> Bool {
        let event = TerminalDropTargetEvent(
            version: 1,
            eventID: UUID().uuidString,
            updatedAt: nowMilliseconds,
            kind: kind,
            screenRow: row,
            column: column,
            text: text,
            references: references
        )
        do {
            let data = try JSONEncoder().encode(event)
            guard data.count <= 1024 * 1024 else { return false }
            try data.write(
                to: sessionDirectory.appendingPathComponent(eventFilename),
                options: .atomic
            )
            return true
        } catch {
            NSLog("[UnpeelNative] failed to publish terminal drop event: %@", error.localizedDescription)
            return false
        }
    }

    static var nowMilliseconds: UInt64 {
        UInt64(max(0, Date().timeIntervalSince1970 * 1_000))
    }
}

struct TerminalDropTargetEvent: Encodable {
    enum Kind: String, Encodable {
        case hover
        case leave
        case drop
    }

    let version: Int
    let eventID: String
    let updatedAt: UInt64
    let kind: Kind
    let screenRow: Int?
    let column: Int?
    let text: String?
    let references: [String]?

    enum CodingKeys: String, CodingKey {
        case version
        case eventID = "event_id"
        case updatedAt = "updated_at"
        case kind
        case screenRow = "screen_row"
        case column
        case text
        case references
    }
}
