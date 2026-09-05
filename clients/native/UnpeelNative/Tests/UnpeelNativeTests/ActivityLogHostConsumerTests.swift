import Foundation
import XCTest
@testable import UnpeelNative

@MainActor
final class ActivityLogHostConsumerTests: XCTestCase {
    func testClientRefreshesHostAppendsWithoutWritingTheFeed() throws {
        let directory = FileManager.default.temporaryDirectory
            .appendingPathComponent("unpeel-activity-consumer-\(UUID().uuidString)")
        try FileManager.default.createDirectory(
            at: directory,
            withIntermediateDirectories: true
        )
        defer { try? FileManager.default.removeItem(at: directory) }
        let file = directory.appendingPathComponent("activity-log.jsonl")
        let store = ActivityLogStore(fileURL: file)
        XCTAssertTrue(store.entries.isEmpty)

        let entry = ActivityLogEntry(
            id: "event-1",
            sessionID: "session-1",
            kind: .finished,
            at: 42,
            title: "A session",
            command: "claude",
            projectID: "project-1",
            projectName: "Project"
        )
        var line = try JSONEncoder().encode(entry)
        line.append(0x0A)
        try line.write(to: file, options: .atomic)
        let before = try Data(contentsOf: file)

        XCTAssertTrue(store.refreshFromHost())
        XCTAssertEqual(store.entries, [entry])
        XCTAssertEqual(try Data(contentsOf: file), before)
        XCTAssertFalse(store.refreshFromHost())

        try FileManager.default.removeItem(at: file)
        XCTAssertTrue(store.refreshFromHost())
        XCTAssertTrue(store.entries.isEmpty)
    }
}
