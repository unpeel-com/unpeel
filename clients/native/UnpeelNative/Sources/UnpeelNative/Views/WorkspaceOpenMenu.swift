//
//  WorkspaceOpenMenu.swift
//  UnpeelNative
//
//  Compact titlebar menu for opening the current project/worktree in
//  external tools. SwiftUI's `Menu` always left-aligns its popup under the
//  label; we want it right-aligned to the (right-anchored) button, so the
//  dropdown is driven by an AppKit `NSMenu` positioned by hand.
//

import AppKit
import SwiftUI

struct WorkspaceOpenMenu: View {
    let preferredTarget: WorkspaceOpenTarget
    let onOpen: (WorkspaceOpenTarget) -> Void

    @State private var controller = WorkspaceMenuController()

    var body: some View {
        WorkspaceOpenSplitButton(
            target: preferredTarget,
            onOpenPreferred: { onOpen(preferredTarget) },
            onShowMenu: {
                controller.onOpen = onOpen
                controller.present()
            },
            controller: controller
        )
        .fixedSize()
    }
}

/// Owns the AppKit menu and the hidden anchor view used to position it so the
/// dropdown's top-right corner lines up with the button's top-right corner.
@MainActor
final class WorkspaceMenuController: NSObject {
    var onOpen: ((WorkspaceOpenTarget) -> Void)?
    weak var anchorView: NSView?

    func present() {
        guard let anchorView else { return }

        let menu = NSMenu()
        menu.autoenablesItems = false
        let groups = WorkspaceOpenTarget.availableMenuGroups()

        for (groupIndex, targets) in groups.enumerated() {
            if groupIndex > 0 {
                menu.addItem(.separator())
            }
            for target in targets {
                let item = NSMenuItem(
                    title: target.title,
                    action: #selector(handleSelection(_:)),
                    keyEquivalent: ""
                )
                item.target = self
                item.representedObject = target.rawValue
                if let icon = WorkspaceAppIconStore.image(for: target)?.copy() as? NSImage {
                    icon.size = NSSize(width: 14, height: 14)
                    item.image = icon
                }
                menu.addItem(item)
            }
        }

        // Anchor view is non-flipped (origin bottom-left). A negative y sits
        // just below the button; offsetting x by the menu width right-aligns
        // the popup to the button's trailing edge.
        let origin = NSPoint(x: anchorView.bounds.maxX - menu.size.width, y: -4)
        menu.popUp(positioning: nil, at: origin, in: anchorView)
    }

    @objc private func handleSelection(_ sender: NSMenuItem) {
        guard
            let raw = sender.representedObject as? String,
            let target = WorkspaceOpenTarget(rawValue: raw)
        else { return }
        onOpen?(target)
    }
}

/// Invisible AppKit view that tracks the button's frame so the menu can be
/// positioned relative to it.
private struct WorkspaceMenuAnchor: NSViewRepresentable {
    let controller: WorkspaceMenuController

    func makeNSView(context: Context) -> NSView {
        let view = NSView()
        controller.anchorView = view
        return view
    }

    func updateNSView(_ nsView: NSView, context: Context) {
        controller.anchorView = nsView
    }
}

private struct WorkspaceOpenSplitButton: View {
    let target: WorkspaceOpenTarget
    let onOpenPreferred: () -> Void
    let onShowMenu: () -> Void
    let controller: WorkspaceMenuController

    @State private var leftHovering = false
    @State private var rightHovering = false

    var body: some View {
        styledControl
            .animation(.easeInOut(duration: 0.12), value: leftHovering)
            .animation(.easeInOut(duration: 0.12), value: rightHovering)
    }

    // Ghost control, like the sidebar toggle and menu buttons on the left of
    // the strip: no fill or border at rest, each half lights up on hover only.
    private var styledControl: some View {
        control
    }

    private var control: some View {
        HStack(spacing: 0) {
            Button(action: onOpenPreferred) {
                WorkspaceAppIconView(target: target, size: 18)
                    .frame(width: 32, height: 26)
                    .background(leftHovering ? Theme.hoverRow : .clear)
                    .contentShape(Rectangle())
            }
            .buttonStyle(.plain)
            .onHover { leftHovering = $0 }
            .help("Open in \(target.title)")
            .accessibilityLabel("Open in \(target.title)")

            divider

            Button(action: onShowMenu) {
                Image(systemName: "chevron.down")
                    .font(.system(size: 9, weight: .semibold))
                    .foregroundStyle(rightHovering ? Theme.foreground : Theme.mutedForeground)
                    .frame(width: 25, height: 26)
                    .background(rightHovering ? Theme.hoverRow : .clear)
                    .contentShape(Rectangle())
            }
            .buttonStyle(.plain)
            .background(WorkspaceMenuAnchor(controller: controller))
            .onHover { rightHovering = $0 }
            .help("Choose app")
            .accessibilityLabel("Choose app")
        }
        .frame(height: 26)
        .clipShape(RoundedRectangle(cornerRadius: 6, style: .continuous))
        .contentShape(RoundedRectangle(cornerRadius: 6, style: .continuous))
    }

    private var divider: some View {
        Rectangle()
            .fill(Theme.foreground.opacity(0.10))
            .frame(width: 1, height: 14)
            .allowsHitTesting(false)
    }
}
