import Foundation
import XCTest
@testable import UnpeelNative

final class TerminalPathDragMapTests: XCTestCase {
    func testFreshMappedCellResolvesAnAbsolutePath() throws {
        let map = TerminalPathDragMap(
            version: 1,
            processID: 42,
            updatedAt: 10_000,
            rows: [
                .init(
                    screenRow: 4,
                    startColumn: 0,
                    endColumn: 18,
                    path: "/tmp/a folder"
                ),
            ]
        )

        XCTAssertEqual(
            map.path(atScreenRow: 4, column: 7, nowMilliseconds: 12_000),
            "/tmp/a folder"
        )
        XCTAssertNil(map.path(atScreenRow: 4, column: 18, nowMilliseconds: 12_000))
        XCTAssertNil(map.path(atScreenRow: 3, column: 7, nowMilliseconds: 12_000))
    }

    func testStaleFutureAndRelativeMapsFailClosed() {
        let row = TerminalPathDragMap.Row(
            screenRow: 1,
            startColumn: 0,
            endColumn: 8,
            path: "/tmp/item"
        )
        XCTAssertNil(
            TerminalPathDragMap(
                version: 1,
                processID: 42,
                updatedAt: 10_000,
                rows: [row]
            ).path(atScreenRow: 1, column: 2, nowMilliseconds: 16_000)
        )
        XCTAssertNil(
            TerminalPathDragMap(
                version: 1,
                processID: 42,
                updatedAt: 20_000,
                rows: [row]
            ).path(atScreenRow: 1, column: 2, nowMilliseconds: 10_000)
        )
        XCTAssertNil(
            TerminalPathDragMap(
                version: 1,
                processID: 42,
                updatedAt: 10_000,
                rows: [
                    .init(
                        screenRow: 1,
                        startColumn: 0,
                        endColumn: 8,
                        path: "relative/item"
                    ),
                ]
            ).path(atScreenRow: 1, column: 2, nowMilliseconds: 10_000)
        )
    }

    func testLoaderRejectsOversizedMarkers() throws {
        let directory = FileManager.default.temporaryDirectory
            .appendingPathComponent(UUID().uuidString, isDirectory: true)
        try FileManager.default.createDirectory(at: directory, withIntermediateDirectories: true)
        defer { try? FileManager.default.removeItem(at: directory) }
        let marker = directory.appendingPathComponent(TerminalPathDragMap.filename)
        try Data(repeating: 0x20, count: Int(TerminalPathDragMap.maximumBytes + 1))
            .write(to: marker)

        XCTAssertNil(TerminalPathDragMap.load(from: directory))
    }
}
