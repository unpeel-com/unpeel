import XCTest
@testable import UnpeelNative

/// A Session's shell runs in one git checkout. Its sidebar row may be filed
/// only at its home (worktree or root) or in a plain group directly under
/// that home — never across a checkout boundary — and the drag, the
/// "Move to ▸" menu, and the project-sidebar pin all read this one rule.
final class SessionMoveRulesTests: XCTestCase {
    private func project(
        _ id: String,
        parent: String? = nil,
        branch: String? = nil,
        group: Bool = false,
        order: Int = 0
    ) -> Project {
        Project(
            id: id,
            name: id,
            path: "/repo",
            pinnedAt: nil,
            parentProjectID: parent,
            sortOrder: order,
            isFolder: group ? true : nil,
            worktreeBranch: branch,
            workspacesEnabled: nil,
            mcpBlocked: nil
        )
    }

    private var catalog: [String: Project] {
        [
            "root": project("root"),
            "group": project("group", parent: "root", group: true, order: 1),
            "wt": project("wt", parent: "root", branch: "feature", order: 2),
            "wt-group": project("wt-group", parent: "wt", group: true),
            "other": project("other"),
        ]
    }

    func testHomeIsTheWorktreeOrTheRoot() {
        XCTAssertEqual(SessionMoveRules.homeProjectID(forProjectID: "wt", projectsByID: catalog), "wt")
        XCTAssertEqual(SessionMoveRules.homeProjectID(forProjectID: "wt-group", projectsByID: catalog), "wt")
        XCTAssertEqual(SessionMoveRules.homeProjectID(forProjectID: "group", projectsByID: catalog), "root")
        XCTAssertEqual(SessionMoveRules.homeProjectID(forProjectID: "root", projectsByID: catalog), "root")
        XCTAssertTrue(SessionMoveRules.isWorktreeBound(sessionProjectID: "wt-group", projectsByID: catalog))
        XCTAssertFalse(SessionMoveRules.isWorktreeBound(sessionProjectID: "group", projectsByID: catalog))
    }

    func testWorktreeSessionFilesOnlyInsideItsWorktree() {
        func can(_ target: String, effective: String = "wt") -> Bool {
            SessionMoveRules.canFile(
                sessionProjectID: "wt",
                effectiveProjectID: effective,
                targetID: target,
                projectsByID: catalog
            )
        }
        XCTAssertTrue(can("wt-group"), "a group inside the worktree is a valid target")
        XCTAssertTrue(can("wt", effective: "wt-group"), "back to the worktree itself")
        XCTAssertFalse(can("wt"), "already there")
        XCTAssertFalse(can("root"), "the parent project is another checkout")
        XCTAssertFalse(can("group"), "a group under the parent is another checkout")
        XCTAssertFalse(can("other"))
        XCTAssertFalse(can("missing"))
    }

    func testRootSessionNeverEntersAWorktree() {
        func can(_ target: String) -> Bool {
            SessionMoveRules.canFile(
                sessionProjectID: "root",
                effectiveProjectID: "root",
                targetID: target,
                projectsByID: catalog
            )
        }
        XCTAssertTrue(can("group"))
        XCTAssertFalse(can("wt"))
        XCTAssertFalse(can("wt-group"))
        XCTAssertFalse(can("other"))
    }

    func testMoveMenuOffersExactlyWhatTheDragAccepts() {
        let worktree = SessionMoveRules.destinations(
            sessionProjectID: "wt",
            effectiveProjectID: "wt",
            projectsByID: catalog,
            isHiddenGroup: { _ in false }
        ).map(\.id)
        XCTAssertEqual(worktree, ["wt-group"])

        let filedInWorktreeGroup = SessionMoveRules.destinations(
            sessionProjectID: "wt",
            effectiveProjectID: "wt-group",
            projectsByID: catalog,
            isHiddenGroup: { _ in false }
        ).map(\.id)
        XCTAssertEqual(filedInWorktreeGroup, ["wt"])

        let root = SessionMoveRules.destinations(
            sessionProjectID: "root",
            effectiveProjectID: "root",
            projectsByID: catalog,
            isHiddenGroup: { $0 == "group" }
        ).map(\.id)
        XCTAssertEqual(root, [], "the hidden storage group never surfaces")
    }

    func testCrossingACheckoutIsRefusedWhileSiblingReorderIsNot() {
        func crosses(_ session: String, over hovered: String) -> Bool {
            SessionMoveRules.crossesCheckout(
                sessionProjectID: session,
                hoveredProjectID: hovered,
                projectsByID: catalog
            )
        }
        // Reordering among siblings inside the worktree (and its groups).
        XCTAssertFalse(crosses("wt", over: "wt"))
        XCTAssertFalse(crosses("wt", over: "wt-group"))
        // Out of the worktree: parent, its group, the root list, another project.
        XCTAssertTrue(crosses("wt", over: "root"))
        XCTAssertTrue(crosses("wt", over: "group"))
        XCTAssertTrue(crosses("wt", over: "other"))
        // A root Session over the worktree's rows is the same boundary.
        XCTAssertTrue(crosses("root", over: "wt"))
        XCTAssertTrue(crosses("group", over: "wt-group"))
        // Two ordinary projects do not shake: that was never a filing target.
        XCTAssertFalse(crosses("root", over: "other"))
    }
}
