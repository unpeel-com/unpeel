import Foundation
import XCTest
@testable import UnpeelNative

final class ResumeCommandTests: XCTestCase {
    func testDecodesHostOwnedRelaunchPlan() throws {
        let data = try JSONSerialization.data(withJSONObject: [
            "command": "agent resume opaque-id",
            "failure_markers": ["missing conversation", "opaque-id"],
        ])
        XCTAssertEqual(
            ResumeCommand.decodeRelaunchPlan(data),
            ResumeCommand.RelaunchPlan(
                command: "agent resume opaque-id",
                failureMarkers: ["missing conversation", "opaque-id"]
            )
        )
    }

    func testOlderHostResponseDefaultsToNoFailureMarkers() throws {
        let data = try JSONSerialization.data(withJSONObject: ["command": "agent --continue"])
        XCTAssertEqual(
            ResumeCommand.decodeRelaunchPlan(data),
            ResumeCommand.RelaunchPlan(command: "agent --continue", failureMarkers: [])
        )
    }

    func testInvalidOrEmptyHostPlanFailsClosed() throws {
        XCTAssertNil(ResumeCommand.decodeRelaunchPlan(Data("{}".utf8)))
        XCTAssertNil(ResumeCommand.decodeRelaunchPlan(Data(#"{"command":""}"#.utf8)))
        XCTAssertNil(ResumeCommand.decodeRelaunchPlan(Data("not-json".utf8)))
    }

    func testManagedStorageCleanupAcceptsOnlyStrictUnpeelDescendants() {
        let root = URL(fileURLWithPath: "/tmp/unpeel-runtime-test", isDirectory: true)
        XCTAssertEqual(
            UnpeelStore.validatedManagedStoragePath(
                "/tmp/unpeel-runtime-test/runtime-storage/session-1",
                unpeelDir: root
            ),
            "/tmp/unpeel-runtime-test/runtime-storage/session-1"
        )
        XCTAssertNil(UnpeelStore.validatedManagedStoragePath(root.path, unpeelDir: root))
        XCTAssertNil(UnpeelStore.validatedManagedStoragePath(
            "/tmp/unpeel-runtime-test-escape/session-1",
            unpeelDir: root
        ))
        XCTAssertNil(UnpeelStore.validatedManagedStoragePath(
            "/tmp/unpeel-runtime-test/../outside",
            unpeelDir: root
        ))
    }
}
