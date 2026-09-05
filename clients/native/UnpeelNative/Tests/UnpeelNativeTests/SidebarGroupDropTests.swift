import XCTest
@testable import UnpeelNative

final class SidebarGroupDropTests: XCTestCase {
    private func project(
        parentID: String?,
        isFolder: Bool?,
        branch: String?
    ) -> Project {
        Project(
            id: "project",
            name: "Project",
            path: "/tmp/project",
            parentProjectID: parentID,
            sortOrder: nil,
            isFolder: isFolder,
            worktreeBranch: branch,
            workspacesEnabled: nil,
            mcpBlocked: nil
        )
    }

    func testOnlyPlainChildGroupsAcceptSessionDrops() {
        XCTAssertTrue(
            project(parentID: "root", isFolder: true, branch: nil).acceptsSessionDrop
        )
        XCTAssertFalse(
            project(parentID: "root", isFolder: nil, branch: "feature").acceptsSessionDrop
        )
        XCTAssertFalse(
            project(parentID: nil, isFolder: true, branch: nil).acceptsSessionDrop
        )
        XCTAssertFalse(
            project(parentID: "root", isFolder: nil, branch: nil).acceptsSessionDrop
        )
    }

    func testFolderSelectionIncludesNestedGroupAndWorktreeSessions() {
        let direct = SessionEntry(
            id: "direct",
            projectID: "group",
            label: "Direct",
            command: "codex",
            createdAt: 0,
            status: .idle
        )
        let nested = SessionEntry(
            id: "nested",
            projectID: "worktree",
            label: "Nested",
            command: "codex",
            createdAt: 0,
            status: .idle
        )
        let worktree = ProjectNode(
            project: Project(
                id: "worktree",
                name: "Worktree",
                path: "/tmp/worktree",
                parentProjectID: "group",
                sortOrder: nil,
                isFolder: nil,
                worktreeBranch: "feature",
                workspacesEnabled: nil,
                mcpBlocked: nil
            ),
            sessions: [nested],
            worktrees: []
        )
        let group = ProjectNode(
            project: Project(
                id: "group",
                name: "Group",
                path: "/tmp/group",
                parentProjectID: "root",
                sortOrder: nil,
                isFolder: true,
                worktreeBranch: nil,
                workspacesEnabled: nil,
                mcpBlocked: nil
            ),
            sessions: [direct],
            worktrees: [worktree]
        )

        XCTAssertTrue(group.containsSidebarSession("direct"))
        XCTAssertTrue(group.containsSidebarSession("nested"))
        XCTAssertFalse(group.containsSidebarSession("elsewhere"))
        XCTAssertFalse(group.containsSidebarSession(nil))
        XCTAssertFalse(worktree.containsSidebarSession("direct"))
        XCTAssertTrue(worktree.containsSidebarSession("nested"))
    }

    @MainActor
    func testSessionDropHighlightClearsWithDragState() {
        let state = SidebarDragState()
        var commits = 0
        var cancels = 0
        state.beginSession(
            projectID: "root",
            sessionID: "session",
            pinned: false,
            commitReorder: { commits += 1 },
            cancelReorder: { cancels += 1 }
        )
        state.setSessionDropTarget("group", hovering: true)
        XCTAssertEqual(state.sessionDropTargetProjectID, "group")

        // An exit from a stale row must not clear the current target.
        state.setSessionDropTarget("other", hovering: false)
        XCTAssertEqual(state.sessionDropTargetProjectID, "group")

        state.end()
        XCTAssertNil(state.sessionDropTargetProjectID)
        XCTAssertNil(state.sessionDrag)
        XCTAssertEqual(commits, 0)
        XCTAssertEqual(cancels, 1)
    }

    /// The detached-drag card keeps the source slot dimmed until it lands,
    /// so the lifted-row marker must survive the drag state's own
    /// `finish()`/`end()` clear — only its owner (the drag controller)
    /// releases it.
    @MainActor
    func testLiftedSessionRowSurvivesDragStateClear() {
        let state = SidebarDragState()
        state.beginSession(
            projectID: "root",
            sessionID: "session",
            pinned: false,
            armed: false,
            commitReorder: {},
            cancelReorder: {}
        )
        state.setLiftedSessionRow("session")
        XCTAssertEqual(state.liftedSessionRowID, "session")

        state.end()
        XCTAssertNil(state.sessionDrag)
        XCTAssertEqual(state.liftedSessionRowID, "session")

        state.setLiftedSessionRow(nil)
        XCTAssertNil(state.liftedSessionRowID)
    }

    /// Top-level projects use the same detached-card lifecycle as sessions:
    /// finishing the order preview must not reveal the source header before
    /// the card's landing animation completes.
    @MainActor
    func testLiftedProjectRowSurvivesDragStateClear() {
        let state = SidebarDragState()
        var commits = 0
        state.beginProject(
            "project",
            armed: false,
            commitReorder: { commits += 1 },
            cancelReorder: {}
        )
        state.setLiftedProjectRow("project")
        XCTAssertEqual(state.liftedProjectRowID, "project")

        state.finish()
        XCTAssertNil(state.projectID)
        XCTAssertEqual(state.liftedProjectRowID, "project")
        XCTAssertEqual(commits, 1)

        state.setLiftedProjectRow(nil)
        XCTAssertNil(state.liftedProjectRowID)
    }

    /// The project drag target survives the tiny LazyVStack gap between two
    /// blocks, but returning over the source cancels the pending reorder.
    func testProjectDropTargetIsStableAcrossGapsAndClearsAtSource() {
        XCTAssertEqual(
            SidebarSessionDragController.retainedProjectDropTarget(
                current: "target", hit: nil, draggedID: "source"
            ),
            "target"
        )
        XCTAssertEqual(
            SidebarSessionDragController.retainedProjectDropTarget(
                current: nil, hit: "target", draggedID: "source"
            ),
            "target"
        )
        XCTAssertNil(
            SidebarSessionDragController.retainedProjectDropTarget(
                current: "target", hit: "source", draggedID: "source"
            )
        )
    }

    func testProjectDropCommitsTheExactVisibleGapFromEitherDirection() {
        let ids = ["a", "b", "c"]
        XCTAssertEqual(
            UnpeelStore.projectInsertionOrder(
                ids: ids, draggedID: "a", targetID: "c", below: false
            ),
            ["b", "a", "c"]
        )
        XCTAssertEqual(
            UnpeelStore.projectInsertionOrder(
                ids: ids, draggedID: "a", targetID: "c", below: true
            ),
            ["b", "c", "a"]
        )
        XCTAssertEqual(
            UnpeelStore.projectInsertionOrder(
                ids: ids, draggedID: "c", targetID: "a", below: false
            ),
            ["c", "a", "b"]
        )
        XCTAssertEqual(
            UnpeelStore.projectInsertionOrder(
                ids: ids, draggedID: "c", targetID: "a", below: true
            ),
            ["a", "c", "b"]
        )
    }

    // MARK: Cross-group positional drop

    /// The insertion gap published while the detached drag hovers another
    /// group's list must clear with the drag state (drop or cancel), never
    /// outlive it.
    @MainActor
    func testSessionInsertionClearsWithDragState() {
        let state = SidebarDragState()
        state.beginSession(
            projectID: "origin",
            sessionID: "session",
            pinned: false,
            armed: false,
            commitReorder: {},
            cancelReorder: {}
        )
        let gap = SidebarDragState.SessionInsertion(
            projectID: "group", anchorID: "anchor", below: true
        )
        state.setSessionInsertion(gap)
        XCTAssertEqual(state.sessionInsertion, gap)

        // Moving to the other half of the same row is a different gap.
        let above = SidebarDragState.SessionInsertion(
            projectID: "group", anchorID: "anchor", below: false
        )
        state.setSessionInsertion(above)
        XCTAssertEqual(state.sessionInsertion, above)

        state.end()
        XCTAssertNil(state.sessionInsertion)

        state.beginSession(
            projectID: "origin",
            sessionID: "session",
            pinned: false,
            armed: false,
            commitReorder: {},
            cancelReorder: {}
        )
        state.setSessionInsertion(gap)
        state.finish()
        XCTAssertNil(state.sessionInsertion)
    }

    /// The composed cross-group drop moves first, then reorders within the
    /// new group: `insertionOrder` computes that final section order from
    /// the post-move ids (dragged already filed into the target list).
    func testInsertionOrderPlacesDraggedAtTheGap() {
        // moveSession appended the dragged session at the end; the drop was
        // released in the gap ABOVE "b".
        XCTAssertEqual(
            SidebarSessionDragController.insertionOrder(
                ids: ["a", "b", "c", "dragged"],
                draggedID: "dragged",
                anchorID: "b",
                below: false
            ),
            ["a", "dragged", "b", "c"]
        )
        // Gap BELOW the anchor.
        XCTAssertEqual(
            SidebarSessionDragController.insertionOrder(
                ids: ["a", "b", "c", "dragged"],
                draggedID: "dragged",
                anchorID: "b",
                below: true
            ),
            ["a", "b", "dragged", "c"]
        )
        // Below the LAST row files at the end of the list.
        XCTAssertEqual(
            SidebarSessionDragController.insertionOrder(
                ids: ["dragged", "a", "b", "c"],
                draggedID: "dragged",
                anchorID: "c",
                below: true
            ),
            ["a", "b", "c", "dragged"]
        )
        // Anchor gone (row vanished between hover and drop): no positional
        // order — the caller keeps the plain moveSession filing.
        XCTAssertNil(
            SidebarSessionDragController.insertionOrder(
                ids: ["a", "b", "dragged"],
                draggedID: "dragged",
                anchorID: "missing",
                below: false
            )
        )
    }

    @MainActor
    func testAcceptedSessionDropCommitsInsteadOfCancelling() {
        let state = SidebarDragState()
        var commits = 0
        var cancels = 0
        state.beginSession(
            projectID: "root",
            sessionID: "session",
            pinned: true,
            commitReorder: { commits += 1 },
            cancelReorder: { cancels += 1 }
        )

        state.finish()

        XCTAssertNil(state.sessionDrag)
        XCTAssertEqual(commits, 1)
        XCTAssertEqual(cancels, 0)
    }
}
