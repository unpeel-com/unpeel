import Foundation

/// Platform-neutral keys understood by the shared focused-row decision table.
public enum UIListNavigationKey: Equatable, Sendable {
    case down
    case up
    case first
    case last
    case pageDown
    case pageUp
    case enter
    case space
    case back
}

public enum UIListNavigationDecision: Equatable, Sendable {
    case down
    case up
    case first
    case last
    case pageDown
    case pageUp
    case invokePrimary
    case back
}

/// One keyboard decision table shared by every native Page/List renderer.
/// Routing remains server-driven; `invokePrimary` only asks the caller to emit
/// the action declared by the current authoritative row.
public func uiListNavigationDecision(
    key: UIListNavigationKey,
    primaryRole: UIListItemPrimaryRole
) -> UIListNavigationDecision? {
    switch key {
    case .enter:
        primaryRole == .static ? nil : .invokePrimary
    case .space:
        primaryRole == .toggle ? .invokePrimary : .pageDown
    case .down: .down
    case .up: .up
    case .first: .first
    case .last: .last
    case .pageDown: .pageDown
    case .pageUp: .pageUp
    case .back: .back
    }
}
