import Foundation
import XCTest
@testable import UnpeelNative

/// Runs every case in the shared `protocol/pane-layout-operations-v1.json`
/// fixture — the normative cross-implementation contract for the split-tree
/// pane model. The TUI's panes module runs the same file; a change that breaks
/// parity fails here and in `cargo test` together.
final class PaneLayoutOperationsConformanceTests: XCTestCase {
    private static let ratioTolerance = 1e-9

    func testRunsSharedPaneLayoutOperationFixture() throws {
        let root = try fixtureJSON("pane-layout-operations-v1.json")
        let cases = try XCTUnwrap(root["cases"] as? [[String: Any]])
        XCTAssertFalse(cases.isEmpty)

        for testCase in cases {
            let caseID = try XCTUnwrap(testCase["id"] as? String)
            try runCase(testCase, caseID: caseID)
        }
    }

    private func runCase(_ testCase: [String: Any], caseID: String) throws {
        let initialJSON = try XCTUnwrap(
            testCase["initial"] as? [String: Any], "\(caseID): initial"
        )
        let initialData = try JSONSerialization.data(withJSONObject: initialJSON)
        // Decode through the real durable codec so v1 initials run the real
        // migration and canonicalize runs on restore.
        let durable = try JSONDecoder().decode(DurablePaneLayout.self, from: initialData)
        var state = durable.restoredState()

        let operations = testCase["operations"] as? [[String: Any]] ?? []
        let expect = try XCTUnwrap(
            testCase["expect"] as? [String: Any], "\(caseID): expect"
        )
        let expectedError = expect["error"] as? String
        var focusResult: String??

        for (index, operation) in operations.enumerated() {
            let isLast = index == operations.count - 1
            do {
                focusResult = try apply(operation, to: &state, caseID: caseID)
                if isLast, let expectedError {
                    XCTFail("\(caseID): expected error \(expectedError), got success")
                }
            } catch let error as PaneLayoutError {
                guard isLast, let expectedError else {
                    XCTFail("\(caseID): unexpected error \(error) at op \(index)")
                    return
                }
                XCTAssertEqual(
                    Self.errorName(error), expectedError,
                    "\(caseID): error kind"
                )
                return
            }
        }
        if expectedError != nil {
            XCTFail("\(caseID): expected error but no operation failed")
            return
        }

        if expect.keys.contains("focusPaneID") {
            let expected = expect["focusPaneID"] as? String
            XCTAssertEqual(
                focusResult ?? nil, expected, "\(caseID): focusPaneID"
            )
        }

        if let layoutJSON = expect["layout"] as? [String: Any] {
            let expectedData = try JSONSerialization.data(withJSONObject: layoutJSON)
            let expected = try JSONDecoder().decode(
                DurablePaneLayout.self, from: expectedData
            )
            let actual = DurablePaneLayout(state: state)
            assertLayoutsEqual(actual, expected, caseID: caseID)
        }

        if let liveLeaves = expect["expectLiveLeaves"] as? [[String: Any]] {
            let actual = state.groups.flatMap(\.panes)
            XCTAssertEqual(actual.count, liveLeaves.count, "\(caseID): live leaf count")
            for (pane, expected) in zip(actual, liveLeaves) {
                XCTAssertEqual(
                    pane.id, expected["paneID"] as? String,
                    "\(caseID): live leaf pane id"
                )
                if let sessionID = expected["sessionID"] as? String {
                    XCTAssertEqual(
                        pane.content.sessionID, sessionID,
                        "\(caseID): live leaf session"
                    )
                }
                if let projectID = expected["launcherProjectID"] as? String {
                    guard case let .launcher(actualProjectID) = pane.content else {
                        XCTFail("\(caseID): expected launcher leaf \(pane.id)")
                        continue
                    }
                    XCTAssertEqual(actualProjectID, projectID, "\(caseID): launcher project")
                }
            }
        }
    }

    /// Applies one fixture operation. Returns the focus query result when the
    /// op was `focusNeighbor`, nil otherwise.
    private func apply(
        _ operation: [String: Any],
        to state: inout PaneLayoutState,
        caseID: String
    ) throws -> String?? {
        let op = operation["op"] as? String ?? ""
        switch op {
        case "insertSession":
            let sessionID = operation["sessionID"] as? String ?? ""
            if let targetPaneID = operation["targetPaneID"] as? String {
                try state.insertSession(
                    sessionID,
                    splitting: targetPaneID,
                    edge: try edge(operation["edge"], caseID: caseID),
                    newPaneID: operation["newPaneID"] as? String
                )
            } else if let groupID = operation["groupID"] as? String {
                try state.insertSession(
                    sessionID,
                    atGroupEdge: try edge(operation["groupEdge"], caseID: caseID),
                    of: groupID,
                    newPaneID: operation["newPaneID"] as? String
                )
            } else {
                try state.insertSession(
                    sessionID,
                    beside: operation["besideSessionID"] as? String ?? "",
                    at: try edge(operation["edge"], caseID: caseID),
                    newGroupID: operation["newGroupID"] as? String,
                    newRepresentativePaneID: operation["newRepresentativePaneID"] as? String,
                    newPaneID: operation["newPaneID"] as? String
                )
            }
        case "insertLauncher":
            try state.insertLauncher(
                projectID: operation["projectID"] as? String ?? "",
                splitting: operation["targetPaneID"] as? String ?? "",
                edge: try edge(operation["edge"], caseID: caseID),
                newPaneID: operation["newPaneID"] as? String
            )
        case "bindLauncher":
            try state.bindLauncher(
                operation["paneID"] as? String ?? "",
                toSessionID: operation["sessionID"] as? String ?? ""
            )
        case "removeLauncher":
            try state.removeLauncher(operation["paneID"] as? String ?? "")
        case "detachPane":
            try state.detachPane(operation["paneID"] as? String ?? "")
        case "closeGroup":
            try state.closeGroup(operation["groupID"] as? String ?? "")
        case "resizeSplit":
            let components = (operation["path"] as? [String] ?? []).compactMap(
                PaneSplitBranch.init(rawValue:)
            )
            try state.resizeSplit(
                in: operation["groupID"] as? String ?? "",
                at: PaneSplitPath(components),
                ratio: operation["ratio"] as? Double ?? .nan
            )
        case "equalize":
            try state.equalize(groupID: operation["groupID"] as? String ?? "")
        case "swapPanes":
            try state.swapPanes(
                operation["paneID"] as? String ?? "",
                with: operation["otherPaneID"] as? String ?? ""
            )
        case "reconcile":
            state.reconcile(eligibleSessionIDs: Set(
                operation["eligibleSessionIDs"] as? [String] ?? []
            ))
        case "focusNeighbor":
            let neighbor = state.spatialNeighbor(
                ofPane: operation["paneID"] as? String ?? "",
                direction: try edge(operation["direction"], caseID: caseID)
            )
            return .some(neighbor?.id)
        default:
            XCTFail("\(caseID): unknown op \(op)")
        }
        return nil
    }

    private func edge(_ value: Any?, caseID: String) throws -> PaneEdge {
        try XCTUnwrap(
            (value as? String).flatMap(PaneEdge.init(rawValue:)),
            "\(caseID): edge \(String(describing: value))"
        )
    }

    private static func errorName(_ error: PaneLayoutError) -> String {
        switch error {
        case .invalidSessionID: return "invalidSessionID"
        case .sameSession: return "sameSession"
        case .duplicateSession: return "duplicateSession"
        case .groupNotFound: return "groupNotFound"
        case .paneNotFound: return "paneNotFound"
        case .splitNotFound: return "splitNotFound"
        case .capacityReached: return "capacityReached"
        case .launcherAlreadyPresent: return "launcherAlreadyPresent"
        case .paneIsNotLauncher: return "paneIsNotLauncher"
        case .panesBelongToDifferentGroups: return "panesBelongToDifferentGroups"
        case .invalidRatio: return "invalidRatio"
        }
    }

    private func assertLayoutsEqual(
        _ actual: DurablePaneLayout,
        _ expected: DurablePaneLayout,
        caseID: String
    ) {
        XCTAssertEqual(actual.groups.count, expected.groups.count, "\(caseID): group count")
        for (actualGroup, expectedGroup) in zip(actual.groups, expected.groups) {
            XCTAssertEqual(actualGroup.id, expectedGroup.id, "\(caseID): group id")
            XCTAssertEqual(
                actualGroup.representativePaneID,
                expectedGroup.representativePaneID,
                "\(caseID): representative"
            )
            assertNodesEqual(
                actualGroup.root, expectedGroup.root,
                caseID: caseID, path: "root"
            )
        }
    }

    private func assertNodesEqual(
        _ actual: DurablePaneNode,
        _ expected: DurablePaneNode,
        caseID: String,
        path: String
    ) {
        switch (actual, expected) {
        case let (.pane(actualID, actualSession), .pane(expectedID, expectedSession)):
            XCTAssertEqual(actualID, expectedID, "\(caseID): \(path) pane id")
            XCTAssertEqual(actualSession, expectedSession, "\(caseID): \(path) session")
        case let (
            .split(actualDirection, actualRatio, actualLeft, actualRight),
            .split(expectedDirection, expectedRatio, expectedLeft, expectedRight)
        ):
            XCTAssertEqual(
                actualDirection, expectedDirection, "\(caseID): \(path) direction"
            )
            XCTAssertEqual(
                actualRatio, expectedRatio,
                accuracy: Self.ratioTolerance,
                "\(caseID): \(path) ratio"
            )
            assertNodesEqual(
                actualLeft, expectedLeft, caseID: caseID, path: "\(path).left"
            )
            assertNodesEqual(
                actualRight, expectedRight, caseID: caseID, path: "\(path).right"
            )
        default:
            XCTFail("\(caseID): \(path) node kind mismatch")
        }
    }

    /// `protocol/<name>` is this checkout's protocol/ directory (the server and
    /// the clients build from one tree);
    /// UNPEEL_PROTOCOL_DIR points at another copy (a server checkout's
    /// `protocol/`, or an extracted archive).
    private func fixtureJSON(_ name: String) throws -> [String: Any] {
        if let override = ProcessInfo.processInfo.environment["UNPEEL_PROTOCOL_DIR"] {
            let data = try Data(contentsOf: URL(fileURLWithPath: override).appendingPathComponent(name))
            return try XCTUnwrap(JSONSerialization.jsonObject(with: data) as? [String: Any])
        }
        var directory = URL(fileURLWithPath: #filePath).deletingLastPathComponent()
        for _ in 0..<10 {
            let candidate = directory
                .appendingPathComponent("protocol")
                .appendingPathComponent(name)
            if FileManager.default.fileExists(atPath: candidate.path) {
                let data = try Data(contentsOf: candidate)
                return try XCTUnwrap(
                    JSONSerialization.jsonObject(with: data) as? [String: Any]
                )
            }
            directory.deleteLastPathComponent()
        }
        XCTFail("could not locate protocol/\(name) from \(#filePath) — set UNPEEL_PROTOCOL_DIR at a protocol/ directory")
        return [:]
    }
}
