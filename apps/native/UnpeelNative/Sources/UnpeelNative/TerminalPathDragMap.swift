//
//  TerminalPathDragMap.swift
//  UnpeelNative
//
//  A terminal App can publish a short-lived map from visible grid rows to
//  Host-local paths in its own hosted-session directory. The native terminal
//  combines that semantic map with its exact point→cell hit test to begin a
//  normal AppKit file-URL drag. This is presentation-only state: it never
//  enters Host manifests, remote protocol state, or durable App state.
//

import Foundation

struct TerminalPathDragMap: Decodable, Equatable {
    struct Row: Decodable, Equatable {
        let screenRow: Int
        let startColumn: Int
        let endColumn: Int
        let path: String

        enum CodingKeys: String, CodingKey {
            case screenRow = "screen_row"
            case startColumn = "start_column"
            case endColumn = "end_column"
            case path
        }
    }

    let version: Int
    let processID: Int32
    let updatedAt: UInt64
    let rows: [Row]

    enum CodingKeys: String, CodingKey {
        case version
        case processID = "pid"
        case updatedAt = "updated_at"
        case rows
    }

    static let filename = "terminal-drag-map.json"
    static let maximumBytes: UInt64 = 64 * 1024
    static let maximumAgeMilliseconds: UInt64 = 5_000
    static let maximumFutureSkewMilliseconds: UInt64 = 5_000

    static func load(from sessionDirectory: URL) -> TerminalPathDragMap? {
        let url = sessionDirectory.appendingPathComponent(filename)
        guard let attributes = try? FileManager.default.attributesOfItem(atPath: url.path),
              let byteCount = (attributes[.size] as? NSNumber)?.uint64Value,
              byteCount <= maximumBytes,
              let data = try? Data(contentsOf: url, options: [.mappedIfSafe])
        else { return nil }
        return try? JSONDecoder().decode(TerminalPathDragMap.self, from: data)
    }

    func path(atScreenRow row: Int, column: Int, nowMilliseconds: UInt64) -> String? {
        guard version == 1,
              processID > 0,
              updatedAt <= nowMilliseconds &+ Self.maximumFutureSkewMilliseconds,
              nowMilliseconds <= updatedAt &+ Self.maximumAgeMilliseconds,
              let match = rows.first(where: {
                  $0.screenRow == row
                      && column >= $0.startColumn
                      && column < $0.endColumn
              }),
              (match.path as NSString).isAbsolutePath
        else { return nil }
        return URL(fileURLWithPath: match.path).standardizedFileURL.path
    }

    static var nowMilliseconds: UInt64 {
        UInt64(max(0, Date().timeIntervalSince1970 * 1_000))
    }
}

