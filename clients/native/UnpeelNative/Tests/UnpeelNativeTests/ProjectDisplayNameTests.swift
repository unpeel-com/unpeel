import XCTest
@testable import UnpeelNative

/// A top-level project's sidebar name is display only (#24): a blank
/// rename falls back to the folder name, never to an empty row.
final class ProjectDisplayNameTests: XCTestCase {
    func testBlankRenameFallsBackToTheFolderName() {
        XCTAssertEqual(
            UnpeelStore.resolvedProjectDisplayName("  \n", path: "/Users/me/src/client-acme-api"),
            "client-acme-api"
        )
    }

    func testRenameIsTrimmed() {
        XCTAssertEqual(
            UnpeelStore.resolvedProjectDisplayName("  API  ", path: "/src/client-acme-api"),
            "API"
        )
    }

    func testFolderNameOfRootPathIsThePath() {
        XCTAssertEqual(UnpeelStore.projectFolderName("/"), "/")
        XCTAssertEqual(UnpeelStore.projectFolderName("/src/web/"), "web")
    }
}
