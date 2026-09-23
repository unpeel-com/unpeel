import XCTest
@testable import UnpeelNative

/// GitHub #26: a group renamed through the Host (Local as a client, the CLI,
/// a phone) lands in app-state.json only; at launch the native records must
/// adopt that name before the mirror pushes records back into the file.
final class NativeProjectNameAdoptionTests: XCTestCase {
    private func record(_ id: String, _ name: String) -> UnpeelStore.NativeProjectRecord {
        UnpeelStore.NativeProjectRecord(
            id: id, name: name, path: "/tmp/p", parentProjectID: "native-parent",
            worktreeBranch: nil, autoDiscoveredWorktree: nil, isFolder: true
        )
    }

    func testSharedNameWinsForKnownEntries() {
        let records = [record("native-g1", "Old group"), record("native-g2", "Unchanged")]
        let shared: [[String: Any]] = [
            ["id": "native-g1", "name": "Renamed group", "path": "/tmp/p"],
            ["id": "native-g2", "name": "Unchanged", "path": "/tmp/p"],
        ]
        let adopted = UnpeelStore.adoptedProjectNames(records: records, sharedProjects: shared)
        XCTAssertEqual(adopted.map(\.name), ["Renamed group", "Unchanged"])
        XCTAssertEqual(adopted.map(\.id), records.map(\.id))
    }

    func testUnknownAndBlankSharedNamesLeaveRecordsAlone() {
        let records = [record("native-new", "Fresh group"), record("native-g3", "Keep me")]
        let shared: [[String: Any]] = [
            ["id": "native-g3", "name": "   ", "path": "/tmp/p"],
            ["id": "native-other", "name": "Someone else", "path": "/tmp/q"],
        ]
        let adopted = UnpeelStore.adoptedProjectNames(records: records, sharedProjects: shared)
        XCTAssertEqual(adopted, records)
    }
}
