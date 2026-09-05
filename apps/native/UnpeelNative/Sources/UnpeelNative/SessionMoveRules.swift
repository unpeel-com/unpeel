import Foundation

/// Where a Session may be filed in the sidebar.
///
/// A Session's shell runs in exactly one checkout. Its HOME is the nearest
/// git-worktree project at or above its manifest project, or the root
/// project when it is not inside a worktree. Filing is display-only (the
/// shared `project-override.json` marker), so it may never claim a different
/// checkout: the only valid targets are the home itself and plain
/// organizational groups directly under it. Reordering among siblings is a
/// different verb and is unaffected.
///
/// The Host enforces the same rule in `controller_host::
/// validate_session_project_target` for every Controller.
enum SessionMoveRules {
    /// The checkout-bound project for a Session whose manifest names
    /// `projectID`: walk up through plain groups until a worktree project or
    /// a root is reached.
    static func homeProjectID(
        forProjectID projectID: String,
        projectsByID: [String: Project]
    ) -> String {
        var id = projectID
        var hops = 0
        while let project = projectsByID[id],
              !project.isWorktree,
              let parent = project.parentProjectID,
              hops < 16 {
            id = parent
            hops += 1
        }
        return id
    }

    /// True when the Session's home is a git worktree: its row may only be
    /// filed inside that worktree.
    static func isWorktreeBound(
        sessionProjectID: String,
        projectsByID: [String: Project]
    ) -> Bool {
        let home = homeProjectID(forProjectID: sessionProjectID, projectsByID: projectsByID)
        return projectsByID[home]?.isWorktree == true
    }

    /// Whether `targetID` is a legal filing destination for a Session whose
    /// manifest names `sessionProjectID` and which currently renders under
    /// `effectiveProjectID` (its override, or the manifest project).
    static func canFile(
        sessionProjectID: String,
        effectiveProjectID: String,
        targetID: String,
        projectsByID: [String: Project]
    ) -> Bool {
        guard let target = projectsByID[targetID] else { return false }
        let home = homeProjectID(forProjectID: sessionProjectID, projectsByID: projectsByID)
        let targetIsHome = targetID == home
        let targetIsPlainGroup = target.acceptsSessionDrop && target.parentProjectID == home
        guard targetIsHome || targetIsPlainGroup else { return false }
        return effectiveProjectID != targetID
    }

    /// "Move to ▸" destinations: the home project plus its plain groups, in
    /// sidebar order, minus the current location and any hidden group.
    static func destinations(
        sessionProjectID: String,
        effectiveProjectID: String,
        projectsByID: [String: Project],
        isHiddenGroup: (String) -> Bool
    ) -> [Project] {
        let homeID = homeProjectID(forProjectID: sessionProjectID, projectsByID: projectsByID)
        guard let home = projectsByID[homeID] else { return [] }
        let groups = projectsByID.values
            .filter {
                $0.parentProjectID == homeID
                    && $0.acceptsSessionDrop
                    && !isHiddenGroup($0.id)
            }
            .sorted { ($0.sortOrder ?? 0) < ($1.sortOrder ?? 0) }
        return ([home] + groups).filter { $0.id != effectiveProjectID }
    }

    /// A drag hovering a row owned by `hoveredProjectID` crosses a checkout
    /// boundary when the two homes differ and at least one of them is a
    /// worktree. Such a release is refused with the "no" shake instead of
    /// silently landing nowhere.
    static func crossesCheckout(
        sessionProjectID: String,
        hoveredProjectID: String,
        projectsByID: [String: Project]
    ) -> Bool {
        let sessionHome = homeProjectID(forProjectID: sessionProjectID, projectsByID: projectsByID)
        let hoveredHome = homeProjectID(forProjectID: hoveredProjectID, projectsByID: projectsByID)
        guard sessionHome != hoveredHome else { return false }
        return projectsByID[sessionHome]?.isWorktree == true
            || projectsByID[hoveredHome]?.isWorktree == true
    }
}
