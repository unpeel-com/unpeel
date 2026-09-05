import XCTest
@testable import UnpeelNative

final class ProjectWorkspaceMoveTests: XCTestCase {
    private var root: URL!

    override func setUpWithError() throws {
        root = FileManager.default.temporaryDirectory
            .appendingPathComponent("unpeel-ws-move-\(UUID().uuidString)", isDirectory: true)
        try FileManager.default.createDirectory(at: root, withIntermediateDirectories: true)
    }

    override func tearDownWithError() throws {
        try? FileManager.default.removeItem(at: root)
        root = nil
    }

    func testMovesProjectSessionsPinsAndOrdersAndLeavesNeighbors() throws {
        let source = root.appendingPathComponent("source", isDirectory: true)
        let dest = root.appendingPathComponent("dest", isDirectory: true)
        try writeAppState(at: source, object: [
            "projects": [
                project("keep", name: "Stay", path: "/tmp/stay"),
                project("move", name: "Flatsome", path: "/tmp/flatsome"),
                project(
                    "group",
                    name: "Research",
                    path: "/tmp/flatsome",
                    parent: "move",
                    folder: true
                ),
            ],
            "pinned_sessions": [
                "move": [pin("sess-move", projectID: "move")],
                "keep": [pin("sess-keep", projectID: "keep")],
            ],
            "session_sort_modes": ["move": "date", "keep": "date"],
            "mcp_orchestrators": [
                "sess-move": ["role": "write", "reach": "project"],
                "sess-keep": ["role": "read", "reach": "project"],
            ],
            "mcp_blocked_projects": ["move", "keep"],
            "presets": [],
        ])
        try writeAppState(at: dest, object: [
            "projects": [project("other", name: "Other", path: "/tmp/other")],
            "pinned_sessions": [:],
            "presets": [],
        ])
        try writeSession(
            home: source,
            id: "sess-move",
            projectID: "move",
            extra: ["hello": true]
        )
        try writeSession(home: source, id: "sess-group", projectID: "move", override: "group")
        try writeSession(home: source, id: "sess-keep", projectID: "keep")
        try writeJSON(
            ["move": ["sess-move"], "keep": ["sess-keep"], "group": ["sess-group"]],
            to: source.appendingPathComponent("session-order.json")
        )
        try writeJSONArray(
            ["keep", "move", "group"],
            to: source.appendingPathComponent("project-order.json")
        )
        try writeJSONArray(
            ["other"],
            to: dest.appendingPathComponent("project-order.json")
        )

        let outcome = try ProjectWorkspaceMove.move(
            projectID: "move",
            from: source,
            to: dest
        )

        XCTAssertEqual(outcome.rootProjectName, "Flatsome")
        XCTAssertEqual(outcome.projectIDs, ["move", "group"])
        XCTAssertEqual(outcome.sessionIDs, ["sess-group", "sess-move"])

        let destState = try loadObject(dest.appendingPathComponent("app-state.json"))
        let destProjects = (destState["projects"] as? [[String: Any]]) ?? []
        XCTAssertEqual(
            Set(destProjects.compactMap { $0["id"] as? String }),
            ["other", "move", "group"]
        )
        let destPins = destState["pinned_sessions"] as? [String: Any]
        XCTAssertNotNil(destPins?["move"])
        XCTAssertNil(destPins?["keep"])
        let destModes = destState["session_sort_modes"] as? [String: String]
        XCTAssertEqual(destModes?["move"], "date")
        let destGrants = destState["mcp_orchestrators"] as? [String: Any]
        XCTAssertNotNil(destGrants?["sess-move"])
        XCTAssertNil(destGrants?["sess-keep"])
        XCTAssertEqual(destState["mcp_blocked_projects"] as? [String], ["move"])

        let sourceState = try loadObject(source.appendingPathComponent("app-state.json"))
        let sourceProjects = (sourceState["projects"] as? [[String: Any]]) ?? []
        XCTAssertEqual(sourceProjects.compactMap { $0["id"] as? String }, ["keep"])
        let sourcePins = sourceState["pinned_sessions"] as? [String: Any]
        XCTAssertNotNil(sourcePins?["keep"])
        XCTAssertNil(sourcePins?["move"])
        XCTAssertEqual(
            (sourceState["mcp_blocked_projects"] as? [String]) ?? [],
            ["keep"]
        )

        XCTAssertTrue(sessionExists(home: dest, id: "sess-move"))
        XCTAssertTrue(sessionExists(home: dest, id: "sess-group"))
        XCTAssertFalse(sessionExists(home: source, id: "sess-move"))
        XCTAssertTrue(sessionExists(home: source, id: "sess-keep"))

        let destOrders = try loadObject(dest.appendingPathComponent("session-order.json"))
        XCTAssertEqual(destOrders["move"] as? [String], ["sess-move"])
        XCTAssertEqual(destOrders["group"] as? [String], ["sess-group"])
        let destProjectOrder = try loadArray(dest.appendingPathComponent("project-order.json"))
        XCTAssertEqual(destProjectOrder, ["other", "move", "group"])
    }

    func testLeavesSessionFiledOutOfTheSubtree() throws {
        let source = root.appendingPathComponent("source", isDirectory: true)
        let dest = root.appendingPathComponent("dest", isDirectory: true)
        try writeAppState(at: source, object: [
            "projects": [
                project("move", name: "A", path: "/tmp/a"),
                project("other", name: "B", path: "/tmp/b"),
            ],
            "presets": [],
        ])
        try writeAppState(at: dest, object: ["projects": [], "presets": []])
        try writeSession(home: source, id: "launched-here", projectID: "move", override: "other")
        try writeSession(home: source, id: "stays-with-a", projectID: "move")

        let outcome = try ProjectWorkspaceMove.move(
            projectID: "move",
            from: source,
            to: dest
        )

        XCTAssertEqual(outcome.sessionIDs, ["stays-with-a"])
        XCTAssertTrue(sessionExists(home: dest, id: "stays-with-a"))
        XCTAssertTrue(sessionExists(home: source, id: "launched-here"))
    }

    func testRefusesDestPathCollisionAndLeavesSourceIntact() throws {
        let source = root.appendingPathComponent("source", isDirectory: true)
        let dest = root.appendingPathComponent("dest", isDirectory: true)
        try writeAppState(at: source, object: [
            "projects": [project("move", name: "A", path: "/tmp/same")],
            "presets": [],
        ])
        try writeAppState(at: dest, object: [
            "projects": [project("existing", name: "Existing", path: "/tmp/same")],
            "presets": [],
        ])
        try writeSession(home: source, id: "sess", projectID: "move")

        XCTAssertThrowsError(
            try ProjectWorkspaceMove.move(projectID: "move", from: source, to: dest)
        ) { error in
            XCTAssertEqual(error as? ProjectWorkspaceMove.MoveError, .destAlreadyHasPath)
        }
        XCTAssertTrue(sessionExists(home: source, id: "sess"))
        XCTAssertFalse(sessionExists(home: dest, id: "sess"))
        let sourceState = try loadObject(source.appendingPathComponent("app-state.json"))
        XCTAssertEqual(
            (sourceState["projects"] as? [[String: Any]])?.compactMap { $0["id"] as? String },
            ["move"]
        )
    }

    func testRefusesDestSessionIdCollision() throws {
        let source = root.appendingPathComponent("source", isDirectory: true)
        let dest = root.appendingPathComponent("dest", isDirectory: true)
        try writeAppState(at: source, object: [
            "projects": [project("move", name: "A", path: "/tmp/a")],
            "presets": [],
        ])
        try writeAppState(at: dest, object: [
            "projects": [project("other", name: "B", path: "/tmp/b")],
            "presets": [],
        ])
        try writeSession(home: source, id: "shared", projectID: "move")
        try writeSession(home: dest, id: "shared", projectID: "other")

        XCTAssertThrowsError(
            try ProjectWorkspaceMove.move(projectID: "move", from: source, to: dest)
        ) { error in
            XCTAssertEqual(
                error as? ProjectWorkspaceMove.MoveError,
                .destAlreadyHasSession("shared")
            )
        }
        XCTAssertTrue(sessionExists(home: source, id: "shared"))
    }

    func testMovesRunningSessionDirWithoutStopping() throws {
        let source = root.appendingPathComponent("source", isDirectory: true)
        let dest = root.appendingPathComponent("dest", isDirectory: true)
        try writeAppState(at: source, object: [
            "projects": [project("move", name: "A", path: "/tmp/a")],
            "presets": [],
        ])
        try writeAppState(at: dest, object: ["projects": [], "presets": []])
        try writeSession(
            home: source,
            id: "live",
            projectID: "move",
            state: "running",
            pid: ProcessInfo.processInfo.processIdentifier
        )

        XCTAssertEqual(
            ProjectWorkspaceMove.liveSessionIDs(projectID: "move", in: source),
            ["live"]
        )
        _ = try ProjectWorkspaceMove.move(projectID: "move", from: source, to: dest)
        XCTAssertFalse(sessionExists(home: source, id: "live"))
        XCTAssertTrue(sessionExists(home: dest, id: "live"))
        let manifest = try JSONSerialization.jsonObject(
            with: Data(
                contentsOf: dest
                    .appendingPathComponent("app-sessions")
                    .appendingPathComponent("live")
                    .appendingPathComponent("manifest.json")
            )
        ) as? [String: Any]
        XCTAssertEqual(manifest?["state"] as? String, "running")
    }

    func testCollectsChildGroupsAndWorktrees() {
        let projects = [
            Project(
                id: "root", name: "R", path: "/r", parentProjectID: nil,
                sortOrder: nil, isFolder: nil, worktreeBranch: nil,
                workspacesEnabled: nil, mcpBlocked: nil
            ),
            Project(
                id: "group", name: "G", path: "/r", parentProjectID: "root",
                sortOrder: nil, isFolder: true, worktreeBranch: nil,
                workspacesEnabled: nil, mcpBlocked: nil
            ),
            Project(
                id: "wt", name: "W", path: "/r-wt", parentProjectID: "root",
                sortOrder: nil, isFolder: nil, worktreeBranch: "feat",
                workspacesEnabled: nil, mcpBlocked: nil
            ),
            Project(
                id: "other", name: "O", path: "/o", parentProjectID: nil,
                sortOrder: nil, isFolder: nil, worktreeBranch: nil,
                workspacesEnabled: nil, mcpBlocked: nil
            ),
        ]
        XCTAssertEqual(
            ProjectWorkspaceMove.descendantProjectIDs(rootID: "root", in: projects),
            ["root", "group", "wt"]
        )
    }

    // MARK: - Fixtures

    private func project(
        _ id: String,
        name: String,
        path: String,
        parent: String? = nil,
        folder: Bool = false
    ) -> [String: Any] {
        var row: [String: Any] = ["id": id, "name": name, "path": path]
        if let parent { row["parent_project_id"] = parent }
        if folder { row["is_folder"] = true }
        return row
    }

    private func pin(_ sessionID: String, projectID: String) -> [String: Any] {
        [
            "key": "session:\(sessionID)",
            "project_id": projectID,
            "session_id": sessionID,
            "pinned_at": 1,
        ]
    }

    private func writeAppState(at home: URL, object: [String: Any]) throws {
        try FileManager.default.createDirectory(at: home, withIntermediateDirectories: true)
        try writeJSON(object, to: home.appendingPathComponent("app-state.json"))
    }

    private func writeSession(
        home: URL,
        id: String,
        projectID: String,
        override: String? = nil,
        extra: [String: Any] = [:],
        state: String = "exited",
        pid: Int32? = nil
    ) throws {
        let dir = home
            .appendingPathComponent("app-sessions", isDirectory: true)
            .appendingPathComponent(id, isDirectory: true)
        try FileManager.default.createDirectory(at: dir, withIntermediateDirectories: true)
        var manifest: [String: Any] = [
            "session": [
                "id": id,
                "project_id": projectID,
            ],
            "state": state,
            "updated_at": 1,
        ]
        if let pid { manifest["pid"] = pid }
        try writeJSON(manifest, to: dir.appendingPathComponent("manifest.json"))
        if !extra.isEmpty {
            try writeJSON(extra, to: dir.appendingPathComponent("extra.json"))
        }
        if let override {
            try writeJSON(
                ["project_id": override, "moved_at": 1],
                to: dir.appendingPathComponent("project-override.json")
            )
        }
    }

    private func sessionExists(home: URL, id: String) -> Bool {
        FileManager.default.fileExists(
            atPath: home
                .appendingPathComponent("app-sessions")
                .appendingPathComponent(id)
                .path
        )
    }

    private func writeJSON(_ object: Any, to url: URL) throws {
        let data = try JSONSerialization.data(withJSONObject: object, options: [.prettyPrinted])
        try FileManager.default.createDirectory(
            at: url.deletingLastPathComponent(),
            withIntermediateDirectories: true
        )
        try data.write(to: url)
    }

    private func writeJSONArray(_ ids: [String], to url: URL) throws {
        try writeJSON(ids, to: url)
    }

    private func loadObject(_ url: URL) throws -> [String: Any] {
        let data = try Data(contentsOf: url)
        return try XCTUnwrap(JSONSerialization.jsonObject(with: data) as? [String: Any])
    }

    private func loadArray(_ url: URL) throws -> [String] {
        let data = try Data(contentsOf: url)
        return try XCTUnwrap(JSONSerialization.jsonObject(with: data) as? [String])
    }
}
