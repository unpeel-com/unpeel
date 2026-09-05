import Foundation
import XCTest
@testable import UnpeelNative

@MainActor
final class PaneLayoutControllerTests: XCTestCase {
    func testInactiveScopeCanBeReadWithoutSwitchingVisibleState() throws {
        let home = temporaryControllerHome()
        defer { try? FileManager.default.removeItem(at: home) }
        let controller = PaneLayoutController(controllerHome: home)

        let local = try controller.mutate { state in
            try state.createGroup(
                representativeSessionID: "local-main",
                adding: "local-member",
                at: .right
            )
        }
        controller.switchScope(to: "host:studio")
        let remote = try controller.mutate { state in
            try state.createGroup(
                representativeSessionID: "remote-main",
                adding: "remote-member",
                at: .right
            )
        }

        let peeked = controller.state(forScopeID: "local")

        XCTAssertEqual(controller.scopeID, "host:studio")
        XCTAssertEqual(controller.state.groups.first?.id, remote.groupID)
        XCTAssertEqual(peeked.groups.first?.id, local.groupID)
        XCTAssertEqual(
            peeked.groups.first?.sessionIDs,
            ["local-main", "local-member"]
        )
    }

    func testScopesKeepCollidingSessionIDsInIndependentLayouts() throws {
        let home = temporaryControllerHome()
        defer { try? FileManager.default.removeItem(at: home) }
        let controller = PaneLayoutController(controllerHome: home)

        let local = try controller.mutate { state in
            try state.createGroup(
                representativeSessionID: "same-representative",
                adding: "same-member",
                at: .right
            )
        }

        controller.switchScope(to: "host:studio")
        XCTAssertTrue(controller.state.groups.isEmpty)
        let remote = try controller.mutate { state in
            try state.createGroup(
                representativeSessionID: "same-representative",
                adding: "same-member",
                at: .left
            )
        }

        controller.switchScope(to: "local")
        XCTAssertEqual(controller.state.groups.first?.id, local.groupID)
        XCTAssertEqual(
            controller.state.groups.first?.sessionIDs,
            ["same-representative", "same-member"]
        )

        controller.switchScope(to: "host:studio")
        XCTAssertEqual(controller.state.groups.first?.id, remote.groupID)
        XCTAssertEqual(
            controller.state.groups.first?.sessionIDs,
            ["same-member", "same-representative"]
        )
    }

    func testWindowsKeepCollidingSessionIDsInIndependentLayouts() throws {
        let home = temporaryControllerHome()
        defer { try? FileManager.default.removeItem(at: home) }
        let fileURL = home.appendingPathComponent(PaneLayoutController.storageFileName)
        // Construct both before either writes. The second save must merge the
        // first window's slot instead of replacing the whole file.
        let main = PaneLayoutController(fileURL: fileURL, windowID: "main")
        let detached = PaneLayoutController(fileURL: fileURL, windowID: "detached-1")

        try main.mutate { state in
            try state.createGroup(
                representativeSessionID: "shared-a",
                adding: "shared-b",
                at: .right
            )
        }
        try detached.mutate { state in
            try state.createGroup(
                representativeSessionID: "shared-a",
                adding: "shared-b",
                at: .left
            )
        }

        let reloadedMain = PaneLayoutController(fileURL: fileURL, windowID: "main")
        let reloadedDetached = PaneLayoutController(
            fileURL: fileURL,
            windowID: "detached-1"
        )
        XCTAssertEqual(
            reloadedMain.state.groups.first?.sessionIDs,
            ["shared-a", "shared-b"]
        )
        XCTAssertEqual(
            reloadedDetached.state.groups.first?.sessionIDs,
            ["shared-b", "shared-a"]
        )
    }

    func testScopeSwitchClearsPresentationRequestsAndRestoresDurableState() throws {
        let home = temporaryControllerHome()
        defer { try? FileManager.default.removeItem(at: home) }
        let controller = PaneLayoutController(controllerHome: home)
        let location = try controller.mutate { state in
            try state.createGroup(
                representativeSessionID: "representative",
                adding: "member",
                at: .right
            )
        }

        controller.setDropTarget(.groupEdge(.right))
        controller.requestFocus(
            groupID: location.groupID,
            paneID: location.paneID,
            sessionID: "member"
        )
        controller.setActivePane(
            groupID: location.groupID,
            paneID: location.paneID,
            sessionID: "member"
        )
        controller.setPendingReveal(groupID: location.groupID, paneID: location.paneID)
        controller.toggleZoom(groupID: location.groupID, paneID: location.paneID)
        XCTAssertEqual(controller.dropTarget, .groupEdge(.right))
        XCTAssertNotNil(controller.focusRequest)
        XCTAssertEqual(controller.activePane?.sessionID, "member")
        XCTAssertEqual(controller.zoomedPane?.paneID, location.paneID)
        XCTAssertEqual(
            controller.pendingRevealPaneID(forGroupID: location.groupID),
            location.paneID
        )
        XCTAssertFalse(
            controller.consumePendingReveal(groupID: location.groupID, paneID: "stale-pane")
        )
        XCTAssertTrue(
            controller.consumePendingReveal(
                groupID: location.groupID,
                paneID: location.paneID
            )
        )
        XCTAssertFalse(
            controller.consumePendingReveal(
                groupID: location.groupID,
                paneID: location.paneID
            )
        )
        controller.setPendingReveal(groupID: location.groupID, paneID: location.paneID)

        controller.switchScope(to: "host:other")
        XCTAssertNil(controller.dropTarget)
        XCTAssertNil(controller.focusRequest)
        XCTAssertNil(controller.pendingReveal)
        XCTAssertNil(controller.activePane)
        XCTAssertNil(controller.zoomedPane)
        XCTAssertTrue(controller.state.groups.isEmpty)

        controller.switchScope(to: "local")
        XCTAssertEqual(controller.state.groups.first?.id, location.groupID)
        XCTAssertNil(controller.dropTarget)
        XCTAssertNil(controller.focusRequest)
        XCTAssertNil(controller.pendingReveal)
        XCTAssertNil(controller.activePane)
        XCTAssertNil(controller.zoomedPane)
    }

    func testZoomTogglesAndClears() {
        let home = temporaryControllerHome()
        defer { try? FileManager.default.removeItem(at: home) }
        let controller = PaneLayoutController(controllerHome: home)

        controller.toggleZoom(groupID: "group", paneID: "pane")
        XCTAssertEqual(
            controller.zoomedPane,
            PaneLayoutController.ZoomedPane(groupID: "group", paneID: "pane")
        )
        // Toggling the same pane un-zooms; toggling another retargets.
        controller.toggleZoom(groupID: "group", paneID: "pane")
        XCTAssertNil(controller.zoomedPane)
        controller.toggleZoom(groupID: "group", paneID: "pane")
        controller.toggleZoom(groupID: "group", paneID: "other")
        XCTAssertEqual(controller.zoomedPane?.paneID, "other")
        controller.clearZoom()
        XCTAssertNil(controller.zoomedPane)
    }

    func testSuccessfulFocusConsumesOnlyTheMatchingNonce() throws {
        let home = temporaryControllerHome()
        defer { try? FileManager.default.removeItem(at: home) }
        let controller = PaneLayoutController(controllerHome: home)

        controller.requestFocus(groupID: "group", paneID: "pane", sessionID: "one")
        let stale = try XCTUnwrap(controller.focusRequest)
        controller.requestFocus(groupID: "group", paneID: "pane", sessionID: "two")
        let current = try XCTUnwrap(controller.focusRequest)

        XCTAssertFalse(controller.consumeFocusRequest(stale))
        XCTAssertEqual(controller.focusRequest, current)
        XCTAssertTrue(controller.consumeFocusRequest(current))
        XCTAssertNil(controller.focusRequest)
    }

    func testUnreadableStorageIsPreservedAndLiveScopeStateSurvivesSwitches() throws {
        let home = temporaryControllerHome()
        defer { try? FileManager.default.removeItem(at: home) }
        try FileManager.default.createDirectory(at: home, withIntermediateDirectories: true)
        let fileURL = home.appendingPathComponent(PaneLayoutController.storageFileName)
        let malformed = Data("{not valid pane state".utf8)
        try malformed.write(to: fileURL)
        let controller = PaneLayoutController(fileURL: fileURL)

        let location = try controller.mutate { state in
            try state.createGroup(
                representativeSessionID: "alpha",
                adding: "beta",
                at: .right
            )
        }
        XCTAssertEqual(try Data(contentsOf: fileURL), malformed)

        controller.switchScope(to: "host:other")
        controller.switchScope(to: "local")
        XCTAssertEqual(controller.state.groups.first?.id, location.groupID)
        XCTAssertEqual(try Data(contentsOf: fileURL), malformed)
    }

    func testFutureSiblingLayoutPreventsLossyEnvelopeRewrite() throws {
        let home = temporaryControllerHome()
        defer { try? FileManager.default.removeItem(at: home) }
        try FileManager.default.createDirectory(at: home, withIntermediateDirectories: true)
        let fileURL = home.appendingPathComponent(PaneLayoutController.storageFileName)
        let future = Data(#"{"version":1,"windows":{"future-window":{"host:future":{"version":99,"groups":[],"future_field":"keep"}}}}"#.utf8)
        try future.write(to: fileURL)
        let controller = PaneLayoutController(fileURL: fileURL)

        try controller.mutate { state in
            try state.createGroup(
                representativeSessionID: "alpha",
                adding: "beta",
                at: .right
            )
        }

        XCTAssertEqual(try Data(contentsOf: fileURL), future)
    }

    func testVersionOneSlotLoadsMigratedAndFirstWriteUpgradesPreservingSiblings() throws {
        let home = temporaryControllerHome()
        defer { try? FileManager.default.removeItem(at: home) }
        try FileManager.default.createDirectory(at: home, withIntermediateDirectories: true)
        let fileURL = home.appendingPathComponent(PaneLayoutController.storageFileName)
        let paneA = "00000000-0000-4000-8000-00000000000a"
        let paneB = "00000000-0000-4000-8000-00000000000b"
        let groupID = "11111111-0000-4000-8000-000000000001"
        let v1 = """
        {"version":1,"windows":{
          "main":{"local":{"version":1,"groups":[{
            "id":"\(groupID)",
            "representativePaneID":"\(paneA)",
            "panes":[
              {"id":"\(paneA)","sessionID":"alpha","fraction":0.6},
              {"id":"\(paneB)","sessionID":"beta","fraction":0.4}
            ]}]}},
          "other-window":{"local":{"version":1,"groups":[]}}
        }}
        """
        try Data(v1.utf8).write(to: fileURL)

        let controller = PaneLayoutController(fileURL: fileURL)
        let group = try XCTUnwrap(controller.state.groups.first)
        XCTAssertEqual(group.id, groupID)
        XCTAssertEqual(group.sessionIDs, ["alpha", "beta"])
        guard case let .split(split) = group.root else {
            return XCTFail("expected migrated split root")
        }
        XCTAssertEqual(split.ratio, 0.6, accuracy: 1e-9)

        // The first mutation writes the slot back as v2 and keeps siblings.
        try controller.mutate { state in
            try state.resizeSplit(in: groupID, at: PaneSplitPath(), ratio: 0.3)
        }
        let json = try XCTUnwrap(
            JSONSerialization.jsonObject(
                with: Data(contentsOf: fileURL)
            ) as? [String: Any]
        )
        let windows = try XCTUnwrap(json["windows"] as? [String: Any])
        XCTAssertNotNil(windows["other-window"])
        let mainSlot = try XCTUnwrap(
            (windows["main"] as? [String: Any])?["local"] as? [String: Any]
        )
        XCTAssertEqual(mainSlot["version"] as? Int, 2)
        let groups = try XCTUnwrap(mainSlot["groups"] as? [[String: Any]])
        XCTAssertNotNil(groups.first?["root"])
    }

    func testDurableRoundTripOmitsLaunchersAndUsesControllerFileEnvelope() throws {
        let home = temporaryControllerHome()
        defer { try? FileManager.default.removeItem(at: home) }
        let controller = PaneLayoutController(
            controllerHome: home,
            windowID: "window-a",
            scopeID: "scope-a"
        )

        try controller.mutate { state in
            try state.createGroup(
                representativeSessionID: "alpha",
                adding: "beta",
                at: .right
            )
        }
        let launcher = try controller.mutate { state in
            try state.insertLauncher(
                projectID: "project",
                beside: "alpha",
                at: .left
            )
        }
        XCTAssertTrue(
            controller.state.groups.flatMap(\.panes).contains(where: { $0.id == launcher.paneID })
        )

        let fileURL = home.appendingPathComponent(PaneLayoutController.storageFileName)
        XCTAssertTrue(FileManager.default.fileExists(atPath: fileURL.path))
        let json = try XCTUnwrap(
            JSONSerialization.jsonObject(with: Data(contentsOf: fileURL)) as? [String: Any]
        )
        XCTAssertEqual(json["version"] as? Int, 1)
        let windows = try XCTUnwrap(json["windows"] as? [String: Any])
        let scopes = try XCTUnwrap(windows["window-a"] as? [String: Any])
        XCTAssertNotNil(scopes["scope-a"])

        let reloaded = PaneLayoutController(
            controllerHome: home,
            windowID: "window-a",
            scopeID: "scope-a"
        )
        let expected = DurablePaneLayout(state: controller.state)
        let restored = DurablePaneLayout(state: reloaded.state)
        XCTAssertEqual(restored.version, expected.version)
        XCTAssertEqual(restored.groups.map(\.id), expected.groups.map(\.id))
        XCTAssertEqual(restored.groups.map(\.root), expected.groups.map(\.root))
        XCTAssertFalse(reloaded.state.groups.flatMap(\.panes).contains(where: {
            $0.content.isLauncher
        }))
        XCTAssertFalse(
            reloaded.state.groups.flatMap(\.panes).contains(where: { $0.id == launcher.paneID })
        )
    }

    private func temporaryControllerHome() -> URL {
        FileManager.default.temporaryDirectory
            .appendingPathComponent("pane-layout-controller-\(UUID().uuidString)", isDirectory: true)
    }
}
