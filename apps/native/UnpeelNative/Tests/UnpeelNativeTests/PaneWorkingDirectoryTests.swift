import XCTest
import UnpeelShared
@testable import UnpeelNative

/// The pane cwd seed behind cmd-click path resolution: a Session's own
/// launch cwd wins, the scope-aware project path is the fallback for Hosts
/// that publish no cwd, and the seed only matters until the shell's OSC 7
/// report replaces it inside the pane.
final class PaneWorkingDirectoryTests: XCTestCase {
    func testScopedSessionWithKnownCwdResolvesARelativePath() {
        let cwd = UnpeelStore.paneWorkingDirectory(
            sessionCwd: "/Users/me/Dev/flatsome",
            projectPath: "/Users/me/Dev/somewhere-else"
        )
        XCTAssertEqual(cwd, "/Users/me/Dev/flatsome")

        var probed: [String] = []
        let resolved = ClickablePath.resolveFile(
            "docs/plans/announcement-bar-single-line.md",
            workingDirectory: cwd,
            fileExists: { path in
                probed.append(path)
                return true
            }
        )
        XCTAssertEqual(
            resolved,
            "/Users/me/Dev/flatsome/docs/plans/announcement-bar-single-line.md"
        )
        XCTAssertEqual(probed, [resolved!])
    }

    func testSessionWithoutCwdFallsBackToItsProjectPath() {
        XCTAssertEqual(
            UnpeelStore.paneWorkingDirectory(
                sessionCwd: nil,
                projectPath: "/Users/me/Dev/flatsome"
            ),
            "/Users/me/Dev/flatsome"
        )
        // A blank cwd from a Host is no cwd at all.
        XCTAssertEqual(
            UnpeelStore.paneWorkingDirectory(
                sessionCwd: "  ",
                projectPath: "/Users/me/Dev/flatsome"
            ),
            "/Users/me/Dev/flatsome"
        )
        XCTAssertNil(UnpeelStore.paneWorkingDirectory(sessionCwd: nil, projectPath: nil))
    }

    func testRelativePathIsUnresolvableWithoutAnyCwd() {
        var probed = false
        XCTAssertNil(ClickablePath.resolveFile(
            "docs/plan.md",
            workingDirectory: nil,
            fileExists: { _ in
                probed = true
                return true
            }
        ))
        XCTAssertFalse(probed, "no cwd must mean no filesystem probe at all")
        // Absolute paths never needed a cwd.
        XCTAssertEqual(
            ClickablePath.resolveFile("/tmp/a.md", workingDirectory: nil, fileExists: { _ in true }),
            "/tmp/a.md"
        )
    }

    func testResolveFileRejectsMissingFilesAndNormalizesDotSegments() {
        XCTAssertNil(ClickablePath.resolveFile(
            "gone.md", workingDirectory: "/x", fileExists: { _ in false }
        ))
        XCTAssertEqual(
            ClickablePath.resolveFile(
                "./docs/../README.md", workingDirectory: "/x/y", fileExists: { _ in true }
            ),
            "/x/y/README.md"
        )
    }

    @MainActor
    func testHostPublishedCwdSurvivesTheSnapshotIntoTheSessionEntry() throws {
        let with = try JSONDecoder().decode(RemoteSessionSummary.self, from: Data("""
        {"id":"a","projectID":"p","title":"a","command":"codex","createdAtUnixMs":1,
         "status":"running","activity":"idle","cwd":"/Users/me/Dev/flatsome"}
        """.utf8))
        XCTAssertEqual(with.cwd, "/Users/me/Dev/flatsome")
        XCTAssertEqual(UnpeelStore.sessionEntry(fromRemote: with).cwd, "/Users/me/Dev/flatsome")

        let without = try JSONDecoder().decode(RemoteSessionSummary.self, from: Data("""
        {"id":"a","projectID":"p","title":"a","command":"codex","createdAtUnixMs":1,
         "status":"running","activity":"idle"}
        """.utf8))
        XCTAssertNil(without.cwd)
        XCTAssertNil(UnpeelStore.sessionEntry(fromRemote: without).cwd)
    }

    func testLocalManifestCwdIsDecoded() throws {
        let manifest = try JSONDecoder().decode(HostedSessionManifest.self, from: Data("""
        {"session":{"id":"a","project_id":"p"},"state":"running",
         "cwd":"/Users/me/Dev/flatsome"}
        """.utf8))
        XCTAssertEqual(manifest.cwd, "/Users/me/Dev/flatsome")
    }
}
