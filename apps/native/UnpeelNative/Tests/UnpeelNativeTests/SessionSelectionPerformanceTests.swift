import Combine
import XCTest
@testable import UnpeelNative

@MainActor
final class SessionSelectionPerformanceTests: XCTestCase {
    func testSelectionPublishesOnlyGlobalAndChangedRowStates() {
        let selection = SessionSelectionState()
        let first = selection.rowState(for: "first")
        let second = selection.rowState(for: "second")
        var globalPublishes = 0
        var firstPublishes = 0
        var secondPublishes = 0
        var cancellables = Set<AnyCancellable>()

        selection.objectWillChange
            .sink { globalPublishes += 1 }
            .store(in: &cancellables)
        first.objectWillChange
            .sink { firstPublishes += 1 }
            .store(in: &cancellables)
        second.objectWillChange
            .sink { secondPublishes += 1 }
            .store(in: &cancellables)

        selection.select("first")
        XCTAssertEqual(globalPublishes, 1)
        XCTAssertEqual(firstPublishes, 1)
        XCTAssertEqual(secondPublishes, 0)
        XCTAssertTrue(first.isSelected)

        selection.select("second")
        XCTAssertEqual(globalPublishes, 2)
        XCTAssertEqual(firstPublishes, 2)
        XCTAssertEqual(secondPublishes, 1)
        XCTAssertFalse(first.isSelected)
        XCTAssertTrue(second.isSelected)

        selection.select("second")
        XCTAssertEqual(globalPublishes, 2, "reselecting is a publication no-op")
        XCTAssertEqual(firstPublishes, 2)
        XCTAssertEqual(secondPublishes, 1)

        selection.select(nil)
        XCTAssertEqual(globalPublishes, 3)
        XCTAssertEqual(firstPublishes, 2)
        XCTAssertEqual(secondPublishes, 2)
        XCTAssertFalse(second.isSelected)
    }

    func testMountedRowStateRetainsStableIdentity() {
        let selection = SessionSelectionState()
        let state = selection.rowState(for: "session")
        XCTAssertTrue(state === selection.rowState(for: "session"))
    }

    func testTitlebarBranchPublishesOnlyInsideItsLeafState() {
        let branch = TitlebarBranchState()
        var publishes = 0
        var cancellables = Set<AnyCancellable>()
        branch.objectWillChange
            .sink { publishes += 1 }
            .store(in: &cancellables)

        branch.update(name: "main", isWorktree: false)
        XCTAssertEqual(publishes, 1)
        XCTAssertEqual(branch.presentation.name, "main")

        branch.update(name: "main", isWorktree: false)
        XCTAssertEqual(publishes, 1, "an unchanged async git result is a no-op")

        branch.update(name: "feature", isWorktree: true)
        XCTAssertEqual(publishes, 2, "name and glyph update in one publication")
        XCTAssertTrue(branch.presentation.isWorktree)
    }

    func testOrdinarySelectionPreservesSidebarListCache() {
        XCTAssertFalse(
            UnpeelStore.selectionChangeAffectsSidebarLists(
                from: "running",
                to: "other-running",
                windowedInactiveSessionIDs: ["stopped", "archived"]
            )
        )
        XCTAssertFalse(
            UnpeelStore.selectionChangeAffectsSidebarLists(
                from: "same",
                to: "same",
                windowedInactiveSessionIDs: ["same"]
            ),
            "a repeated selection cannot change list membership"
        )
    }

    func testInactiveSelectionInvalidatesOnlyWhenPreviewMembershipCanChange() {
        XCTAssertTrue(
            UnpeelStore.selectionChangeAffectsSidebarLists(
                from: "running",
                to: "stopped",
                windowedInactiveSessionIDs: ["stopped", "archived"]
            )
        )
        XCTAssertTrue(
            UnpeelStore.selectionChangeAffectsSidebarLists(
                from: "running",
                to: "archived",
                windowedInactiveSessionIDs: ["stopped", "archived"]
            )
        )
        XCTAssertTrue(
            UnpeelStore.selectionChangeAffectsSidebarLists(
                from: "archived",
                to: "stopped",
                windowedInactiveSessionIDs: ["stopped", "archived"]
            )
        )
        XCTAssertFalse(
            UnpeelStore.selectionChangeAffectsSidebarLists(
                from: "running",
                to: "archived",
                windowedInactiveSessionIDs: []
            ),
            "an in-flight archive row is already forced visible"
        )
    }
}
