import CoreGraphics
import Foundation

// The recursive split-tree pane model. The normative cross-implementation
// contract — operation semantics, durable schema, v1 migration, and the
// spatial/equalize algorithms — is `protocol/pane-layout-operations-v1.json`;
// the TUI's panes module implements the same contract.

enum PaneContent: Equatable, Sendable {
    case session(id: String)
    case launcher(projectID: String)

    var sessionID: String? {
        guard case let .session(id) = self else { return nil }
        return id
    }

    var isLauncher: Bool {
        if case .launcher = self { return true }
        return false
    }
}

struct Pane: Identifiable, Equatable, Sendable {
    let id: String
    var content: PaneContent

    init(id: String = PaneStableID.make(), content: PaneContent) {
        self.id = PaneStableID.canonical(id) ?? PaneStableID.make()
        self.content = content
    }
}

enum SplitDirection: String, Equatable, Sendable {
    case horizontal
    case vertical
}

/// A drop/split edge. `left`/`up` place the new leaf as the left (leading/top)
/// child; `right`/`down` as the right (trailing/bottom) child.
enum PaneEdge: String, Equatable, Sendable {
    case left
    case right
    case up
    case down

    var splitDirection: SplitDirection {
        switch self {
        case .left, .right: return .horizontal
        case .up, .down: return .vertical
        }
    }

    var newLeafIsLeftChild: Bool {
        switch self {
        case .left, .up: return true
        case .right, .down: return false
        }
    }
}

/// Where a dragged Session would land: splitting a specific pane on one of
/// its four edges, or splitting the whole group's root at a content edge.
/// Presentation state only — never persisted.
enum PaneDropTarget: Equatable, Sendable {
    case pane(paneID: String, edge: PaneEdge)
    case groupEdge(PaneEdge)
}

enum PaneSplitBranch: String, Equatable, Sendable {
    case left
    case right
}

/// Addresses a split node inside a group's tree. The empty path is the root.
struct PaneSplitPath: Equatable, Sendable {
    var components: [PaneSplitBranch]

    init(_ components: [PaneSplitBranch] = []) {
        self.components = components
    }
}

struct PaneSplit: Equatable, Sendable {
    var direction: SplitDirection
    var ratio: Double {
        didSet { ratio = Self.clampedRatio(ratio) }
    }
    var left: PaneNode
    var right: PaneNode

    init(direction: SplitDirection, ratio: Double, left: PaneNode, right: PaneNode) {
        self.direction = direction
        self.ratio = Self.clampedRatio(ratio)
        self.left = left
        self.right = right
    }

    static func clampedRatio(_ ratio: Double) -> Double {
        guard ratio.isFinite else { return 0.5 }
        return min(max(ratio, PaneLayoutState.minimumSplitRatio), PaneLayoutState.maximumSplitRatio)
    }
}

indirect enum PaneNode: Equatable, Sendable {
    case leaf(Pane)
    case split(PaneSplit)
}

extension PaneNode {
    /// Leaves in preorder (left-first). Preorder is the normative leaf order:
    /// it drives sidebar rows, representative promotion, and cap trimming.
    var leaves: [Pane] {
        switch self {
        case let .leaf(pane):
            return [pane]
        case let .split(split):
            return split.left.leaves + split.right.leaves
        }
    }

    var sessionLeaves: [Pane] {
        leaves.filter { $0.content.sessionID != nil }
    }

    var containsLauncher: Bool {
        leaves.contains { $0.content.isLauncher }
    }

    func leaf(id paneID: String) -> Pane? {
        leaves.first { $0.id == paneID }
    }

    func path(toPane paneID: String) -> PaneSplitPath? {
        switch self {
        case let .leaf(pane):
            return pane.id == paneID ? PaneSplitPath() : nil
        case let .split(split):
            if let left = split.left.path(toPane: paneID) {
                return PaneSplitPath([.left] + left.components)
            }
            if let right = split.right.path(toPane: paneID) {
                return PaneSplitPath([.right] + right.components)
            }
            return nil
        }
    }

    func node(at path: PaneSplitPath) -> PaneNode? {
        var current = self
        for component in path.components {
            guard case let .split(split) = current else { return nil }
            current = component == .left ? split.left : split.right
        }
        return current
    }

    func replacingNode(at path: PaneSplitPath, with node: PaneNode) -> PaneNode {
        guard let first = path.components.first else { return node }
        guard case let .split(split) = self else { return self }
        var updated = split
        let rest = PaneSplitPath(Array(path.components.dropFirst()))
        if first == .left {
            updated.left = split.left.replacingNode(at: rest, with: node)
        } else {
            updated.right = split.right.replacingNode(at: rest, with: node)
        }
        return .split(updated)
    }

    /// Removes a leaf; the surviving sibling replaces its parent split, so
    /// single-child splits never exist. Returns nil when the tree empties.
    func removingLeaf(_ paneID: String) -> PaneNode? {
        switch self {
        case let .leaf(pane):
            return pane.id == paneID ? nil : self
        case let .split(split):
            if case let .leaf(pane) = split.left, pane.id == paneID {
                return split.right
            }
            if case let .leaf(pane) = split.right, pane.id == paneID {
                return split.left
            }
            var updated = split
            if let left = split.left.removingLeaf(paneID) {
                updated.left = left
            } else {
                return split.right
            }
            if let right = split.right.removingLeaf(paneID) {
                updated.right = right
            } else {
                return split.left
            }
            return .split(updated)
        }
    }

    func updatingLeaf(_ paneID: String, _ transform: (inout Pane) -> Void) -> PaneNode {
        switch self {
        case var .leaf(pane):
            guard pane.id == paneID else { return self }
            transform(&pane)
            return .leaf(pane)
        case var .split(split):
            split.left = split.left.updatingLeaf(paneID, transform)
            split.right = split.right.updatingLeaf(paneID, transform)
            return .split(split)
        }
    }

    /// Wraps the target leaf in a 0.5 split with the new leaf on the edge side.
    func splittingLeaf(_ paneID: String, adding pane: Pane, edge: PaneEdge) -> PaneNode? {
        guard let path = path(toPane: paneID), let target = node(at: path) else {
            return nil
        }
        let split = PaneSplit(
            direction: edge.splitDirection,
            ratio: 0.5,
            left: edge.newLeafIsLeftChild ? .leaf(pane) : target,
            right: edge.newLeafIsLeftChild ? target : .leaf(pane)
        )
        return replacingNode(at: path, with: .split(split))
    }

    /// Equalizes every split: ratio = leftWeight / totalWeight, where a child
    /// weighs 1 (leaf or perpendicular split) or the sum of its same-direction
    /// children's weights.
    func equalized() -> PaneNode {
        switch self {
        case .leaf:
            return self
        case let .split(split):
            let leftWeight = split.left.weight(forDirection: split.direction)
            let rightWeight = split.right.weight(forDirection: split.direction)
            let ratio = Double(leftWeight) / Double(leftWeight + rightWeight)
            return .split(PaneSplit(
                direction: split.direction,
                ratio: ratio,
                left: split.left.equalized(),
                right: split.right.equalized()
            ))
        }
    }

    private func weight(forDirection direction: SplitDirection) -> Int {
        switch self {
        case .leaf:
            return 1
        case let .split(split):
            guard split.direction == direction else { return 1 }
            return split.left.weight(forDirection: direction)
                + split.right.weight(forDirection: direction)
        }
    }

    /// Structure-only identity: tree shape, directions, leaf ids, and leaf
    /// content — deliberately excluding ratios so divider drags never change
    /// SwiftUI identity (retained terminal surfaces must not remount).
    var structuralIdentity: Int {
        var hasher = Hasher()
        hashStructure(into: &hasher)
        return hasher.finalize()
    }

    private func hashStructure(into hasher: inout Hasher) {
        switch self {
        case let .leaf(pane):
            hasher.combine(0)
            hasher.combine(pane.id)
            switch pane.content {
            case let .session(id):
                hasher.combine(id)
            case let .launcher(projectID):
                hasher.combine("launcher:\(projectID)")
            }
        case let .split(split):
            hasher.combine(1)
            hasher.combine(split.direction.rawValue)
            split.left.hashStructure(into: &hasher)
            split.right.hashStructure(into: &hasher)
        }
    }

    // MARK: Spatial layout

    /// Grid dimensions: leaf = 1x1; a horizontal split sums widths and maxes
    /// heights; a vertical split sums heights and maxes widths.
    var gridDimensions: (width: Double, height: Double) {
        switch self {
        case .leaf:
            return (1, 1)
        case let .split(split):
            let left = split.left.gridDimensions
            let right = split.right.gridDimensions
            switch split.direction {
            case .horizontal:
                return (left.width + right.width, max(left.height, right.height))
            case .vertical:
                return (max(left.width, right.width), left.height + right.height)
            }
        }
    }

    /// Leaf rects from recursive ratio subdivision of `bounds`; (0,0) is
    /// top-left, y grows down, and a vertical split's left child is on top.
    /// Preorder emission is load-bearing: it is the stable tie-break for
    /// spatial navigation.
    func leafSlots(in bounds: CGRect) -> [(pane: Pane, bounds: CGRect)] {
        switch self {
        case let .leaf(pane):
            return [(pane, bounds)]
        case let .split(split):
            let leftBounds: CGRect
            let rightBounds: CGRect
            switch split.direction {
            case .horizontal:
                leftBounds = CGRect(
                    x: bounds.minX,
                    y: bounds.minY,
                    width: bounds.width * split.ratio,
                    height: bounds.height
                )
                rightBounds = CGRect(
                    x: bounds.minX + bounds.width * split.ratio,
                    y: bounds.minY,
                    width: bounds.width * (1 - split.ratio),
                    height: bounds.height
                )
            case .vertical:
                leftBounds = CGRect(
                    x: bounds.minX,
                    y: bounds.minY,
                    width: bounds.width,
                    height: bounds.height * split.ratio
                )
                rightBounds = CGRect(
                    x: bounds.minX,
                    y: bounds.minY + bounds.height * split.ratio,
                    width: bounds.width,
                    height: bounds.height * (1 - split.ratio)
                )
            }
            return split.left.leafSlots(in: leftBounds)
                + split.right.leafSlots(in: rightBounds)
        }
    }

    /// The neighboring leaf in a direction, using artificial grid-dimension
    /// bounds so the answer is a pure function of the tree. Candidates must
    /// clear the reference's edge; nearest top-left-corner distance wins,
    /// preorder breaks ties.
    func spatialNeighbor(of paneID: String, direction: PaneEdge) -> Pane? {
        let dimensions = gridDimensions
        let slots = leafSlots(in: CGRect(
            x: 0, y: 0, width: dimensions.width, height: dimensions.height
        ))
        guard let reference = slots.first(where: { $0.pane.id == paneID }) else {
            return nil
        }
        var best: (pane: Pane, distance: Double)?
        for slot in slots where slot.pane.id != paneID {
            let qualifies: Bool
            switch direction {
            case .left: qualifies = slot.bounds.maxX <= reference.bounds.minX
            case .right: qualifies = slot.bounds.minX >= reference.bounds.maxX
            case .up: qualifies = slot.bounds.maxY <= reference.bounds.minY
            case .down: qualifies = slot.bounds.minY >= reference.bounds.maxY
            }
            guard qualifies else { continue }
            let dx = Double(slot.bounds.minX - reference.bounds.minX)
            let dy = Double(slot.bounds.minY - reference.bounds.minY)
            let distance = (dx * dx + dy * dy).squareRoot()
            if best == nil || distance < best!.distance {
                best = (slot.pane, distance)
            }
        }
        return best?.pane
    }
}

struct PaneGroup: Identifiable, Equatable, Sendable {
    let id: String
    var representativePaneID: String
    var root: PaneNode
    /// The exact tree from before a transient launcher was inserted, restored
    /// on cancel. Presentation-only; never durable.
    var preLauncherRoot: PaneNode?

    init(
        id: String = PaneStableID.make(),
        representativePaneID: String,
        root: PaneNode,
        preLauncherRoot: PaneNode? = nil
    ) {
        self.id = PaneStableID.canonical(id) ?? PaneStableID.make()
        self.representativePaneID = PaneStableID.canonical(representativePaneID)
            ?? representativePaneID
        self.root = root
        self.preLauncherRoot = preLauncherRoot
    }

    /// Leaves in preorder — the sidebar/visual enumeration order.
    var panes: [Pane] {
        root.leaves
    }

    var sessionIDs: [String] {
        root.leaves.compactMap(\.content.sessionID)
    }

    var representativeSessionID: String? {
        root.leaf(id: representativePaneID)?.content.sessionID
    }
}

struct PaneLocation: Equatable, Sendable {
    let groupID: String
    let paneID: String
}

struct PaneLayoutChange: Equatable, Sendable {
    let groupID: String
    let removedPaneIDs: [String]
    let releasedSessionIDs: [String]
    let representativePaneID: String?
    let dissolved: Bool
}

enum PaneLayoutError: Error, Equatable {
    case invalidSessionID
    case sameSession
    case duplicateSession(String)
    case groupNotFound(String)
    case paneNotFound(String)
    case splitNotFound(String)
    case capacityReached(String)
    case launcherAlreadyPresent(String)
    case paneIsNotLauncher(String)
    case panesBelongToDifferentGroups
    case invalidRatio
}

struct PaneLayoutState: Equatable, Sendable {
    static let maximumSessionLeafCount = 8
    static let minimumSplitRatio = 0.1
    static let maximumSplitRatio = 0.9

    private(set) var groups: [PaneGroup]

    init(groups: [PaneGroup] = []) {
        self.groups = []
        canonicalize(groups)
    }

    // MARK: Queries

    func group(containingSession sessionID: String) -> PaneGroup? {
        guard let location = location(ofSession: sessionID) else { return nil }
        return groups.first(where: { $0.id == location.groupID })
    }

    func group(id groupID: String) -> PaneGroup? {
        groups.first(where: { $0.id == groupID })
    }

    func pane(id paneID: String) -> Pane? {
        for group in groups {
            if let pane = group.root.leaf(id: paneID) {
                return pane
            }
        }
        return nil
    }

    func location(ofSession sessionID: String) -> PaneLocation? {
        for group in groups {
            if let pane = group.root.leaves.first(where: {
                $0.content.sessionID == sessionID
            }) {
                return PaneLocation(groupID: group.id, paneID: pane.id)
            }
        }
        return nil
    }

    func location(ofPane paneID: String) -> PaneLocation? {
        for group in groups where group.root.leaf(id: paneID) != nil {
            return PaneLocation(groupID: group.id, paneID: paneID)
        }
        return nil
    }

    func spatialNeighbor(ofPane paneID: String, direction: PaneEdge) -> Pane? {
        guard let location = location(ofPane: paneID),
              let group = group(id: location.groupID)
        else { return nil }
        return group.root.spatialNeighbor(of: paneID, direction: direction)
    }

    // MARK: Insert

    @discardableResult
    mutating func createGroup(
        representativeSessionID: String,
        adding sessionID: String,
        at edge: PaneEdge,
        newGroupID: String? = nil,
        newRepresentativePaneID: String? = nil,
        newPaneID: String? = nil
    ) throws -> PaneLocation {
        try validateNewSession(sessionID)
        guard !representativeSessionID.isEmpty else {
            throw PaneLayoutError.invalidSessionID
        }
        guard representativeSessionID != sessionID else {
            throw PaneLayoutError.sameSession
        }
        if location(ofSession: representativeSessionID) != nil {
            throw PaneLayoutError.duplicateSession(representativeSessionID)
        }

        let representative = Pane(
            id: newRepresentativePaneID ?? PaneStableID.make(),
            content: .session(id: representativeSessionID)
        )
        let inserted = Pane(
            id: newPaneID ?? PaneStableID.make(),
            content: .session(id: sessionID)
        )
        let split = PaneSplit(
            direction: edge.splitDirection,
            ratio: 0.5,
            left: edge.newLeafIsLeftChild ? .leaf(inserted) : .leaf(representative),
            right: edge.newLeafIsLeftChild ? .leaf(representative) : .leaf(inserted)
        )
        let group = PaneGroup(
            id: newGroupID ?? PaneStableID.make(),
            representativePaneID: representative.id,
            root: .split(split)
        )
        groups.append(group)
        return PaneLocation(groupID: group.id, paneID: inserted.id)
    }

    /// Splits the target session's leaf, or creates a group when that session
    /// is currently solo.
    @discardableResult
    mutating func insertSession(
        _ sessionID: String,
        beside targetSessionID: String,
        at edge: PaneEdge,
        newGroupID: String? = nil,
        newRepresentativePaneID: String? = nil,
        newPaneID: String? = nil
    ) throws -> PaneLocation {
        try validateNewSession(sessionID)
        guard !targetSessionID.isEmpty else {
            throw PaneLayoutError.invalidSessionID
        }
        guard sessionID != targetSessionID else {
            throw PaneLayoutError.sameSession
        }

        if let target = location(ofSession: targetSessionID) {
            return try insertSession(
                sessionID,
                splitting: target.paneID,
                edge: edge,
                newPaneID: newPaneID
            )
        }
        return try createGroup(
            representativeSessionID: targetSessionID,
            adding: sessionID,
            at: edge,
            newGroupID: newGroupID,
            newRepresentativePaneID: newRepresentativePaneID,
            newPaneID: newPaneID
        )
    }

    @discardableResult
    mutating func insertSession(
        _ sessionID: String,
        splitting targetPaneID: String,
        edge: PaneEdge,
        newPaneID: String? = nil
    ) throws -> PaneLocation {
        try validateNewSession(sessionID)
        guard let location = location(ofPane: targetPaneID) else {
            throw PaneLayoutError.paneNotFound(targetPaneID)
        }
        let groupIndex = try index(ofGroup: location.groupID)
        try validateCapacity(ofGroupAt: groupIndex)

        let pane = Pane(
            id: newPaneID ?? PaneStableID.make(),
            content: .session(id: sessionID)
        )
        guard let root = groups[groupIndex].root.splittingLeaf(
            targetPaneID, adding: pane, edge: edge
        ) else {
            throw PaneLayoutError.paneNotFound(targetPaneID)
        }
        // Adding a real Session while a launcher is open is an intentional
        // layout mutation; its geometry becomes the new basis.
        groups[groupIndex].preLauncherRoot = nil
        groups[groupIndex].root = root
        return PaneLocation(groupID: groups[groupIndex].id, paneID: pane.id)
    }

    /// Splits the group's root; the new leaf's share is 1/(sessionLeafCount+1).
    @discardableResult
    mutating func insertSession(
        _ sessionID: String,
        atGroupEdge edge: PaneEdge,
        of groupID: String,
        newPaneID: String? = nil
    ) throws -> PaneLocation {
        try validateNewSession(sessionID)
        let groupIndex = try index(ofGroup: groupID)
        try validateCapacity(ofGroupAt: groupIndex)

        let pane = Pane(
            id: newPaneID ?? PaneStableID.make(),
            content: .session(id: sessionID)
        )
        let share = 1.0 / Double(groups[groupIndex].root.sessionLeaves.count + 1)
        let split = PaneSplit(
            direction: edge.splitDirection,
            ratio: edge.newLeafIsLeftChild ? share : 1.0 - share,
            left: edge.newLeafIsLeftChild ? .leaf(pane) : groups[groupIndex].root,
            right: edge.newLeafIsLeftChild ? groups[groupIndex].root : .leaf(pane)
        )
        groups[groupIndex].preLauncherRoot = nil
        groups[groupIndex].root = .split(split)
        return PaneLocation(groupID: groups[groupIndex].id, paneID: pane.id)
    }

    // MARK: Launcher lifecycle

    @discardableResult
    mutating func insertLauncher(
        projectID: String,
        splitting targetPaneID: String,
        edge: PaneEdge,
        newPaneID: String? = nil
    ) throws -> PaneLocation {
        guard let location = location(ofPane: targetPaneID) else {
            throw PaneLayoutError.paneNotFound(targetPaneID)
        }
        let groupIndex = try index(ofGroup: location.groupID)
        try validateCapacity(ofGroupAt: groupIndex)
        guard !groups[groupIndex].root.containsLauncher else {
            throw PaneLayoutError.launcherAlreadyPresent(location.groupID)
        }

        let launcher = Pane(
            id: newPaneID ?? PaneStableID.make(),
            content: .launcher(projectID: projectID)
        )
        let snapshot = groups[groupIndex].root
        guard let root = snapshot.splittingLeaf(
            targetPaneID, adding: launcher, edge: edge
        ) else {
            throw PaneLayoutError.paneNotFound(targetPaneID)
        }
        groups[groupIndex].preLauncherRoot = snapshot
        groups[groupIndex].root = root
        return PaneLocation(groupID: groups[groupIndex].id, paneID: launcher.id)
    }

    /// Splits the target session's leaf, or creates a session + launcher group
    /// when that session is currently solo.
    @discardableResult
    mutating func insertLauncher(
        projectID: String,
        beside targetSessionID: String,
        at edge: PaneEdge,
        newGroupID: String? = nil,
        newRepresentativePaneID: String? = nil,
        newPaneID: String? = nil
    ) throws -> PaneLocation {
        guard !targetSessionID.isEmpty else {
            throw PaneLayoutError.invalidSessionID
        }

        if let target = location(ofSession: targetSessionID) {
            return try insertLauncher(
                projectID: projectID,
                splitting: target.paneID,
                edge: edge,
                newPaneID: newPaneID
            )
        }

        let representative = Pane(
            id: newRepresentativePaneID ?? PaneStableID.make(),
            content: .session(id: targetSessionID)
        )
        let launcher = Pane(
            id: newPaneID ?? PaneStableID.make(),
            content: .launcher(projectID: projectID)
        )
        let split = PaneSplit(
            direction: edge.splitDirection,
            ratio: 0.5,
            left: edge.newLeafIsLeftChild ? .leaf(launcher) : .leaf(representative),
            right: edge.newLeafIsLeftChild ? .leaf(representative) : .leaf(launcher)
        )
        let group = PaneGroup(
            id: newGroupID ?? PaneStableID.make(),
            representativePaneID: representative.id,
            root: .split(split)
        )
        groups.append(group)
        return PaneLocation(groupID: group.id, paneID: launcher.id)
    }

    mutating func bindLauncher(_ paneID: String, toSessionID sessionID: String) throws {
        try validateNewSession(sessionID)
        guard let location = location(ofPane: paneID) else {
            throw PaneLayoutError.paneNotFound(paneID)
        }
        let groupIndex = try index(ofGroup: location.groupID)
        guard groups[groupIndex].root.leaf(id: paneID)?.content.isLauncher == true else {
            throw PaneLayoutError.paneIsNotLauncher(paneID)
        }
        groups[groupIndex].root = groups[groupIndex].root.updatingLeaf(paneID) {
            $0.content = .session(id: sessionID)
        }
        groups[groupIndex].preLauncherRoot = nil
    }

    @discardableResult
    mutating func removeLauncher(_ paneID: String) throws -> PaneLayoutChange {
        guard let location = location(ofPane: paneID) else {
            throw PaneLayoutError.paneNotFound(paneID)
        }
        let groupIndex = try index(ofGroup: location.groupID)
        guard groups[groupIndex].root.leaf(id: paneID)?.content.isLauncher == true else {
            throw PaneLayoutError.paneIsNotLauncher(paneID)
        }
        return try detachPane(paneID)
    }

    // MARK: Detach / close

    @discardableResult
    mutating func detachPane(_ paneID: String) throws -> PaneLayoutChange {
        guard let location = location(ofPane: paneID) else {
            throw PaneLayoutError.paneNotFound(paneID)
        }
        let groupIndex = try index(ofGroup: location.groupID)
        let original = groups[groupIndex]
        guard let removedPane = original.root.leaf(id: paneID) else {
            throw PaneLayoutError.paneNotFound(paneID)
        }

        guard let newRoot = original.root.removingLeaf(paneID) else {
            groups.remove(at: groupIndex)
            return PaneLayoutChange(
                groupID: original.id,
                removedPaneIDs: original.panes.map(\.id),
                releasedSessionIDs: original.sessionIDs,
                representativePaneID: nil,
                dissolved: true
            )
        }

        let sessionLeaves = newRoot.sessionLeaves
        let canRemainGrouped = sessionLeaves.count >= 2
            || (sessionLeaves.count == 1 && newRoot.containsLauncher)
        guard canRemainGrouped else {
            groups.remove(at: groupIndex)
            return PaneLayoutChange(
                groupID: original.id,
                removedPaneIDs: original.panes.map(\.id),
                releasedSessionIDs: original.sessionIDs,
                representativePaneID: nil,
                dissolved: true
            )
        }

        var updated = original
        if removedPane.content.isLauncher {
            // Cancel restores the snapshot only when it still describes
            // exactly the surviving session leaves.
            if let snapshot = updated.preLauncherRoot,
               Set(snapshot.sessionLeaves.map(\.id)) == Set(sessionLeaves.map(\.id))
            {
                updated.root = snapshot
            } else {
                updated.root = newRoot
            }
            updated.preLauncherRoot = nil
        } else {
            updated.root = newRoot
            if let snapshot = updated.preLauncherRoot {
                let pruned = snapshot.removingLeaf(paneID)
                updated.preLauncherRoot = (pruned?.sessionLeaves.count ?? 0) >= 2
                    ? pruned
                    : nil
            }
        }
        if updated.root.leaf(id: updated.representativePaneID)?.content.sessionID == nil {
            updated.representativePaneID = updated.root.sessionLeaves[0].id
        }
        groups[groupIndex] = updated
        return PaneLayoutChange(
            groupID: original.id,
            removedPaneIDs: [paneID],
            releasedSessionIDs: removedPane.content.sessionID.map { [$0] } ?? [],
            representativePaneID: updated.representativePaneID,
            dissolved: false
        )
    }

    @discardableResult
    mutating func closeGroup(_ groupID: String) throws -> PaneLayoutChange {
        let groupIndex = try index(ofGroup: groupID)
        let group = groups.remove(at: groupIndex)
        return PaneLayoutChange(
            groupID: group.id,
            removedPaneIDs: group.panes.map(\.id),
            releasedSessionIDs: group.sessionIDs,
            representativePaneID: nil,
            dissolved: true
        )
    }

    // MARK: Geometry

    @discardableResult
    mutating func resizeSplit(
        in groupID: String,
        at path: PaneSplitPath,
        ratio: Double
    ) throws -> Double {
        guard ratio.isFinite else { throw PaneLayoutError.invalidRatio }
        let groupIndex = try index(ofGroup: groupID)
        guard let node = groups[groupIndex].root.node(at: path),
              case var .split(split) = node
        else {
            throw PaneLayoutError.splitNotFound(groupID)
        }
        let applied = PaneSplit.clampedRatio(ratio)
        guard split.ratio != applied else { return applied }
        split.ratio = applied
        // Resizing while a launcher is visible is explicit user intent, so a
        // later cancel must not resurrect the pre-resize geometry.
        groups[groupIndex].preLauncherRoot = nil
        groups[groupIndex].root = groups[groupIndex].root.replacingNode(
            at: path, with: .split(split)
        )
        return applied
    }

    mutating func equalize(groupID: String) throws {
        let groupIndex = try index(ofGroup: groupID)
        groups[groupIndex].preLauncherRoot = nil
        groups[groupIndex].root = groups[groupIndex].root.equalized()
    }

    /// Exchanges the positions of two leaves in the same group. Pane ids
    /// travel with their leaves, so the representative id is unaffected.
    @discardableResult
    mutating func swapPanes(_ paneID: String, with otherPaneID: String) throws -> Bool {
        guard let source = location(ofPane: paneID) else {
            throw PaneLayoutError.paneNotFound(paneID)
        }
        guard let target = location(ofPane: otherPaneID) else {
            throw PaneLayoutError.paneNotFound(otherPaneID)
        }
        guard source.groupID == target.groupID else {
            throw PaneLayoutError.panesBelongToDifferentGroups
        }
        guard paneID != otherPaneID else { return false }

        let groupIndex = try index(ofGroup: source.groupID)
        let root = groups[groupIndex].root
        guard let first = root.leaf(id: paneID),
              let second = root.leaf(id: otherPaneID),
              let firstPath = root.path(toPane: paneID),
              let secondPath = root.path(toPane: otherPaneID)
        else {
            throw PaneLayoutError.paneNotFound(paneID)
        }
        // Replace by position, not by id: paths stay valid because only leaf
        // payloads change, never the tree shape.
        groups[groupIndex].preLauncherRoot = nil
        groups[groupIndex].root = root
            .replacingNode(at: firstPath, with: .leaf(second))
            .replacingNode(at: secondPath, with: .leaf(first))
        return true
    }

    // MARK: Reconcile

    /// Drops sessions no longer eligible, collapses around them, promotes the
    /// first remaining session leaf when the representative disappears, and
    /// dissolves non-transient groups with fewer than two sessions. A session
    /// + launcher pair remains while its launcher interaction is active.
    @discardableResult
    mutating func reconcile(
        eligibleSessionIDs: Set<String>
    ) -> [PaneLayoutChange] {
        var reconciled: [PaneGroup] = []
        var changes: [PaneLayoutChange] = []

        for original in groups {
            let ineligible = original.root.sessionLeaves.filter { pane in
                guard let sessionID = pane.content.sessionID else { return false }
                return !eligibleSessionIDs.contains(sessionID)
            }
            var newRoot: PaneNode? = original.root
            for pane in ineligible {
                newRoot = newRoot?.removingLeaf(pane.id)
            }

            let sessionLeaves = newRoot?.sessionLeaves ?? []
            let hasLauncher = newRoot?.containsLauncher ?? false
            let canRemainGrouped = sessionLeaves.count >= 2
                || (sessionLeaves.count == 1 && hasLauncher)

            guard let newRoot, canRemainGrouped else {
                changes.append(PaneLayoutChange(
                    groupID: original.id,
                    removedPaneIDs: original.panes.map(\.id),
                    releasedSessionIDs: original.sessionIDs,
                    representativePaneID: nil,
                    dissolved: true
                ))
                continue
            }

            var updated = original
            updated.root = newRoot
            if updated.root.leaf(id: updated.representativePaneID)?.content.sessionID == nil {
                updated.representativePaneID = sessionLeaves[0].id
            }
            if let snapshot = updated.preLauncherRoot, hasLauncher {
                var pruned: PaneNode? = snapshot
                for pane in ineligible {
                    pruned = pruned?.removingLeaf(pane.id)
                }
                let prunedLeafIDs = Set((pruned?.sessionLeaves ?? []).map(\.id))
                let liveLeafIDs = Set(sessionLeaves.map(\.id))
                updated.preLauncherRoot = (pruned?.sessionLeaves.count ?? 0) >= 2
                    && prunedLeafIDs == liveLeafIDs
                    ? pruned
                    : nil
            } else {
                updated.preLauncherRoot = nil
            }
            reconciled.append(updated)

            if updated != original {
                let retainedPaneIDs = Set(updated.panes.map(\.id))
                changes.append(PaneLayoutChange(
                    groupID: original.id,
                    removedPaneIDs: original.panes.map(\.id).filter {
                        !retainedPaneIDs.contains($0)
                    },
                    releasedSessionIDs: original.sessionIDs.filter {
                        !eligibleSessionIDs.contains($0)
                    },
                    representativePaneID: updated.representativePaneID,
                    dissolved: false
                ))
            }
        }

        groups = reconciled
        return changes
    }

    // MARK: Canonicalization

    private mutating func canonicalize(_ candidates: [PaneGroup]) {
        var usedGroupIDs = Set<String>()
        var usedPaneIDs = Set<String>()
        var usedSessionIDs = Set<String>()

        for candidate in candidates {
            var groupID = candidate.id
            if usedGroupIDs.contains(groupID) {
                groupID = PaneStableID.make()
            }

            // Drop invalid leaves: empty/duplicate sessions, duplicate pane
            // ids, and any launcher after the first.
            var candidatePaneIDs = usedPaneIDs
            var candidateSessionIDs = usedSessionIDs
            var hasLauncher = false
            var root: PaneNode? = candidate.root
            for pane in candidate.root.leaves {
                var keep = !candidatePaneIDs.contains(pane.id)
                if keep {
                    switch pane.content {
                    case let .session(sessionID):
                        keep = !sessionID.isEmpty
                            && !candidateSessionIDs.contains(sessionID)
                        if keep { candidateSessionIDs.insert(sessionID) }
                    case .launcher:
                        keep = !hasLauncher
                        hasLauncher = hasLauncher || keep
                    }
                }
                if keep {
                    candidatePaneIDs.insert(pane.id)
                } else {
                    root = root?.removingLeaf(pane.id)
                }
            }

            // Enforce the session-leaf cap by trimming trailing preorder
            // leaves.
            while let current = root,
                  current.sessionLeaves.count > Self.maximumSessionLeafCount,
                  let last = current.sessionLeaves.last
            {
                candidatePaneIDs.remove(last.id)
                if let sessionID = last.content.sessionID {
                    candidateSessionIDs.remove(sessionID)
                }
                root = current.removingLeaf(last.id)
            }

            guard let root else { continue }
            let sessionLeaves = root.sessionLeaves
            let canRemainGrouped = sessionLeaves.count >= 2
                || (sessionLeaves.count == 1 && hasLauncher)
            guard canRemainGrouped else { continue }

            usedGroupIDs.insert(groupID)
            usedPaneIDs = candidatePaneIDs
            usedSessionIDs = candidateSessionIDs

            let representative = root.leaf(
                id: candidate.representativePaneID
            )?.content.sessionID != nil
                ? candidate.representativePaneID
                : sessionLeaves[0].id
            let snapshot: PaneNode? = {
                guard hasLauncher, let snapshot = candidate.preLauncherRoot else {
                    return nil
                }
                let snapshotIDs = Set(snapshot.sessionLeaves.map(\.id))
                let liveIDs = Set(sessionLeaves.map(\.id))
                return snapshotIDs == liveIDs && snapshotIDs.count >= 2
                    ? snapshot
                    : nil
            }()
            groups.append(PaneGroup(
                id: groupID,
                representativePaneID: representative,
                root: root,
                preLauncherRoot: snapshot
            ))
        }
    }

    // MARK: Helpers

    private func validateNewSession(_ sessionID: String) throws {
        guard !sessionID.isEmpty else { throw PaneLayoutError.invalidSessionID }
        if location(ofSession: sessionID) != nil {
            throw PaneLayoutError.duplicateSession(sessionID)
        }
    }

    private func validateCapacity(ofGroupAt groupIndex: Int) throws {
        // A live launcher counts prospectively so a later bind cannot push the
        // group past the cap.
        let prospective = groups[groupIndex].root.sessionLeaves.count
            + (groups[groupIndex].root.containsLauncher ? 1 : 0)
        guard prospective < Self.maximumSessionLeafCount else {
            throw PaneLayoutError.capacityReached(groups[groupIndex].id)
        }
    }

    private func index(ofGroup groupID: String) throws -> Int {
        guard let index = groups.firstIndex(where: { $0.id == groupID }) else {
            throw PaneLayoutError.groupNotFound(groupID)
        }
        return index
    }
}

// MARK: - Durable codec

/// The Codable projection of a layout. Launchers and groups that do not
/// contain at least two sessions are omitted; while a launcher is live and a
/// pre-launcher snapshot exists, the snapshot is what gets encoded. Writes are
/// always version 2; version 1 (the flat pane list) migrates on read.
struct DurablePaneLayout: Codable, Equatable, Sendable {
    static let currentVersion = 2
    static let supportedVersions = 1...2

    let version: Int
    let groups: [DurablePaneGroup]

    init(state: PaneLayoutState) {
        version = Self.currentVersion
        groups = state.groups.compactMap(DurablePaneGroup.init(group:))
    }

    func restoredState() -> PaneLayoutState {
        PaneLayoutState(groups: groups.map { durable in
            PaneGroup(
                id: durable.id,
                representativePaneID: durable.representativePaneID,
                root: durable.root.paneNode()
            )
        })
    }

    private enum CodingKeys: String, CodingKey {
        case version
        case groups
    }

    init(from decoder: Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        let decodedVersion = try container.decode(Int.self, forKey: .version)
        version = decodedVersion
        switch decodedVersion {
        case 2:
            groups = try container.decode([DurablePaneGroup].self, forKey: .groups)
        case 1:
            let legacy = try container.decode(
                [LegacyDurablePaneGroup].self, forKey: .groups
            )
            groups = legacy.compactMap { $0.migrated() }
        default:
            // Unknown future version: capture the version so writers can fail
            // closed; the content is not preserved.
            groups = []
        }
    }

    func encode(to encoder: Encoder) throws {
        var container = encoder.container(keyedBy: CodingKeys.self)
        try container.encode(Self.currentVersion, forKey: .version)
        try container.encode(groups, forKey: .groups)
    }
}

struct DurablePaneGroup: Codable, Equatable, Sendable {
    let id: String
    let representativePaneID: String
    let root: DurablePaneNode

    init?(group: PaneGroup) {
        // Persist the pre-launcher snapshot while a launcher interaction is
        // live; otherwise strip launcher leaves via the removal algorithm.
        var candidate: PaneNode?
        if group.root.containsLauncher, let snapshot = group.preLauncherRoot {
            candidate = snapshot
        } else {
            candidate = group.root
            for pane in group.root.leaves where pane.content.isLauncher {
                candidate = candidate?.removingLeaf(pane.id)
            }
        }
        guard let candidate, candidate.sessionLeaves.count >= 2 else {
            return nil
        }
        id = group.id
        representativePaneID = candidate.leaf(
            id: group.representativePaneID
        ) != nil
            ? group.representativePaneID
            : candidate.sessionLeaves[0].id
        root = DurablePaneNode(candidate)
    }

    init(id: String, representativePaneID: String, root: DurablePaneNode) {
        self.id = id
        self.representativePaneID = representativePaneID
        self.root = root
    }
}

indirect enum DurablePaneNode: Codable, Equatable, Sendable {
    case pane(id: String, sessionID: String)
    case split(
        direction: SplitDirection,
        ratio: Double,
        left: DurablePaneNode,
        right: DurablePaneNode
    )

    init(_ node: PaneNode) {
        switch node {
        case let .leaf(pane):
            self = .pane(id: pane.id, sessionID: pane.content.sessionID ?? "")
        case let .split(split):
            self = .split(
                direction: split.direction,
                ratio: split.ratio,
                left: DurablePaneNode(split.left),
                right: DurablePaneNode(split.right)
            )
        }
    }

    func paneNode() -> PaneNode {
        switch self {
        case let .pane(id, sessionID):
            return .leaf(Pane(id: id, content: .session(id: sessionID)))
        case let .split(direction, ratio, left, right):
            return .split(PaneSplit(
                direction: direction,
                ratio: ratio,
                left: left.paneNode(),
                right: right.paneNode()
            ))
        }
    }

    private enum CodingKeys: String, CodingKey {
        case pane
        case split
    }

    private struct PanePayload: Codable {
        let id: String
        let sessionID: String
    }

    private struct SplitPayload: Codable {
        let direction: String
        let ratio: Double
        let left: DurablePaneNode
        let right: DurablePaneNode
    }

    init(from decoder: Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        if let pane = try container.decodeIfPresent(PanePayload.self, forKey: .pane) {
            self = .pane(id: pane.id, sessionID: pane.sessionID)
            return
        }
        let split = try container.decode(SplitPayload.self, forKey: .split)
        guard let direction = SplitDirection(rawValue: split.direction) else {
            throw DecodingError.dataCorruptedError(
                forKey: .split,
                in: container,
                debugDescription: "Unknown split direction \(split.direction)"
            )
        }
        self = .split(
            direction: direction,
            ratio: split.ratio,
            left: split.left,
            right: split.right
        )
    }

    func encode(to encoder: Encoder) throws {
        var container = encoder.container(keyedBy: CodingKeys.self)
        switch self {
        case let .pane(id, sessionID):
            try container.encode(
                PanePayload(id: id, sessionID: sessionID), forKey: .pane
            )
        case let .split(direction, ratio, left, right):
            try container.encode(
                SplitPayload(
                    direction: direction.rawValue,
                    ratio: ratio,
                    left: left,
                    right: right
                ),
                forKey: .split
            )
        }
    }
}

/// The version-1 flat shape, decoded only for migration. Accepts both the
/// Swift (`sessionID`/`representativePaneID`) and legacy Rust
/// (`sessionId`/`representativePaneId`) key spellings.
private struct LegacyDurablePaneGroup: Decodable {
    let id: String
    let representativePaneID: String
    let panes: [LegacyDurablePane]

    private enum CodingKeys: String, CodingKey {
        case id
        case representativePaneID
        case representativePaneId
        case panes
    }

    init(from decoder: Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        id = try container.decode(String.self, forKey: .id)
        if let value = try container.decodeIfPresent(
            String.self, forKey: .representativePaneID
        ) {
            representativePaneID = value
        } else {
            representativePaneID = try container.decode(
                String.self, forKey: .representativePaneId
            )
        }
        panes = try container.decode([LegacyDurablePane].self, forKey: .panes)
    }

    /// Folds the flat pane list into a right-leaning horizontal chain:
    /// node(i) = split(horizontal, clamp(f_i / (f_i + … + f_n)), leaf(p_i),
    /// node(i+1)). Divide, then clamp.
    func migrated() -> DurablePaneGroup? {
        guard panes.count >= 2 else { return nil }

        func fold(_ index: Int) -> DurablePaneNode {
            let pane = panes[index]
            let leaf = DurablePaneNode.pane(id: pane.id, sessionID: pane.sessionID)
            guard index < panes.count - 1 else { return leaf }
            let remaining = panes[index...].reduce(0.0) { partial, pane in
                partial + (pane.fraction.isFinite ? max(0, pane.fraction) : 0)
            }
            let fraction = pane.fraction.isFinite ? max(0, pane.fraction) : 0
            let ratio = remaining > 0
                ? PaneSplit.clampedRatio(fraction / remaining)
                : 0.5
            return .split(
                direction: .horizontal,
                ratio: ratio,
                left: leaf,
                right: fold(index + 1)
            )
        }

        return DurablePaneGroup(
            id: id,
            representativePaneID: representativePaneID,
            root: fold(0)
        )
    }
}

private struct LegacyDurablePane: Decodable {
    let id: String
    let sessionID: String
    let fraction: Double

    private enum CodingKeys: String, CodingKey {
        case id
        case sessionID
        case sessionId
        case fraction
    }

    init(from decoder: Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        id = try container.decode(String.self, forKey: .id)
        if let value = try container.decodeIfPresent(String.self, forKey: .sessionID) {
            sessionID = value
        } else {
            sessionID = try container.decode(String.self, forKey: .sessionId)
        }
        fraction = try container.decode(Double.self, forKey: .fraction)
    }
}

enum PaneStableID {
    static func make() -> String {
        UUID().uuidString.lowercased()
    }

    static func canonical(_ value: String) -> String? {
        UUID(uuidString: value)?.uuidString.lowercased()
    }
}
