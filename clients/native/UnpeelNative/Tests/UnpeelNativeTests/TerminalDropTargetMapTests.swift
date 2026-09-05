import Foundation
import Testing
@testable import UnpeelNative

struct TerminalDropTargetMapTests {
    @Test func targetHitTestingIsFreshBoundedAndHalfOpen() throws {
        let now = TerminalDropTargetMap.nowMilliseconds
        let data = try JSONSerialization.data(withJSONObject: [
            "version": 1,
            "pid": 42,
            "updated_at": now,
            "regions": [[
                "screen_row": 3,
                "start_column": 2,
                "end_row": 8,
                "end_column": 20,
            ]],
        ])
        let map = try JSONDecoder().decode(TerminalDropTargetMap.self, from: data)
        #expect(map.accepts(row: 3, column: 2, nowMilliseconds: now))
        #expect(map.accepts(row: 7, column: 19, nowMilliseconds: now))
        #expect(!map.accepts(row: 8, column: 19, nowMilliseconds: now))
        #expect(!map.accepts(row: 7, column: 20, nowMilliseconds: now))
        #expect(!map.accepts(
            row: 4,
            column: 4,
            nowMilliseconds: now + TerminalDropTargetMap.maximumAgeMilliseconds + 1
        ))
    }

    @Test func eventWriterUsesTheSharedWireContract() throws {
        let directory = FileManager.default.temporaryDirectory
            .appendingPathComponent(UUID().uuidString, isDirectory: true)
        try FileManager.default.createDirectory(at: directory, withIntermediateDirectories: true)
        defer { try? FileManager.default.removeItem(at: directory) }

        #expect(TerminalDropTargetMap.writeEvent(
            kind: .drop,
            row: 5,
            column: 9,
            text: "notes/file.md",
            references: ["/tmp/notes/file.md"],
            to: directory
        ))
        let data = try Data(contentsOf: directory.appendingPathComponent(
            TerminalDropTargetMap.eventFilename
        ))
        let event = try #require(JSONSerialization.jsonObject(with: data) as? [String: Any])
        #expect(event["version"] as? Int == 1)
        #expect(event["kind"] as? String == "drop")
        #expect(event["screen_row"] as? Int == 5)
        #expect(event["column"] as? Int == 9)
        #expect(event["text"] as? String == "notes/file.md")
    }
}
