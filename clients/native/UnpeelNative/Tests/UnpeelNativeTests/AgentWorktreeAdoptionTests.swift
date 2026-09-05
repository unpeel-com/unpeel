import XCTest
@testable import UnpeelNative

/// Settings ▸ Worktrees ▸ "Show agent worktrees" drives the sidebar tree
/// through the native project records: ON reconciles a Git discovery pass
/// into adopted child-folder rows, OFF purges every adopted row while
/// explicitly registered worktrees and groups stay.
final class AgentWorktreeAdoptionTests: XCTestCase {
    private func project(_ id: String, path: String, parent: String? = nil, branch: String? = nil) -> Project {
        Project(
            id: id,
            name: id,
            path: path,
            pinnedAt: nil,
            parentProjectID: parent,
            sortOrder: 0,
            isFolder: nil,
            worktreeBranch: branch,
            workspacesEnabled: nil,
            mcpBlocked: nil
        )
    }

    private let repo = "/tmp/unpeel-adoption-tests/repo"
    private let agentCheckout = "/tmp/unpeel-adoption-tests/repo-agent-1"
    private let explicitCheckout = "/tmp/unpeel-adoption-tests/repo-feature"

    private var explicitWorktree: UnpeelStore.NativeProjectRecord {
        UnpeelStore.NativeProjectRecord(
            id: "native-explicit",
            name: "feature",
            path: explicitCheckout,
            parentProjectID: "root",
            worktreeBranch: "feature"
        )
    }

    private var group: UnpeelStore.NativeProjectRecord {
        UnpeelStore.NativeProjectRecord(
            id: "native-group",
            name: "Ideas",
            path: repo,
            parentProjectID: "root",
            isFolder: true
        )
    }

    func testTurningOnAdoptsTheAgentCheckoutAsAChildFolder() {
        let projects = [project("root", path: repo)]
        let adopted = UnpeelStore.reconcilingAutoDiscoveredWorktrees(
            listed: ["root": [
                WorktreeGit.LinkedWorktree(path: agentCheckout, branch: "claude/idea-1"),
            ]],
            projects: projects,
            records: [explicitWorktree, group]
        )
        let rows = try! XCTUnwrap(adopted)
        XCTAssertEqual(rows.count, 3)
        let row = try! XCTUnwrap(rows.first { $0.autoDiscoveredWorktree == true })
        XCTAssertTrue(row.id.hasPrefix("native-auto-worktree-"))
        XCTAssertEqual(row.parentProjectID, "root")
        XCTAssertEqual(row.worktreeBranch, "claude/idea-1")
        XCTAssertEqual(row.name, "repo-agent-1", "the folder name reads better than the branch")

        // A second identical pass changes nothing (no republish churn).
        XCTAssertNil(UnpeelStore.reconcilingAutoDiscoveredWorktrees(
            listed: ["root": [
                WorktreeGit.LinkedWorktree(path: agentCheckout, branch: "claude/idea-1"),
            ]],
            projects: projects,
            records: rows
        ))
    }

    func testTurningOffPurgesAdoptedRowsAndKeepsEverythingElse() {
        let adopted = UnpeelStore.reconcilingAutoDiscoveredWorktrees(
            listed: ["root": [WorktreeGit.LinkedWorktree(path: agentCheckout, branch: "x")]],
            projects: [project("root", path: repo)],
            records: [explicitWorktree, group]
        )!
        let purged = try! XCTUnwrap(UnpeelStore.purgingAutoDiscoveredWorktrees(adopted))
        XCTAssertEqual(purged, [explicitWorktree, group])
        // Nothing adopted → nothing to write.
        XCTAssertNil(UnpeelStore.purgingAutoDiscoveredWorktrees(purged))
    }

    func testAFailedListingNeverForgetsAnAdoptedRowButAnEmptyOneDoes() {
        let projects = [project("root", path: repo)]
        let adopted = UnpeelStore.reconcilingAutoDiscoveredWorktrees(
            listed: ["root": [WorktreeGit.LinkedWorktree(path: agentCheckout, branch: "x")]],
            projects: projects,
            records: []
        )!
        XCTAssertNil(
            UnpeelStore.reconcilingAutoDiscoveredWorktrees(listed: [:], projects: projects, records: adopted),
            "a parent absent from the listing (git failed) keeps its children"
        )
        XCTAssertEqual(
            UnpeelStore.reconcilingAutoDiscoveredWorktrees(listed: ["root": []], projects: projects, records: adopted),
            [],
            "a successful empty listing removes the stale adopted row"
        )
    }
}
