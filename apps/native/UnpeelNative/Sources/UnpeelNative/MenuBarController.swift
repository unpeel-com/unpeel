//
//  MenuBarController.swift
//  UnpeelNative
//
//  macOS menu-bar (NSStatusItem) presence for the activity dropdown. Lets the
//  user close the main window while the app keeps running: hosted sessions
//  retain their live spinner here, and clicking a session re-opens the window
//  and smooth-scrolls to it. The dropdown body is the SAME `ActivityMenuList`
//  the in-app titlebar popover uses, so the two surfaces stay identical.
//
//  The status button is driven via its native `title`/`image` (a timer swaps
//  braille spinner frames). An earlier version hosted a SwiftUI subview pinned
//  to the button's edges, which grew the button's bounds and anchored the
//  popover far down the screen — driving the button directly keeps it the
//  normal menu-bar size so the popover sits right beneath it.
//

import AppKit
import Combine
import SwiftUI

@MainActor
final class MenuBarController: NSObject {
    private let store: UnpeelStore
    private let activityModel: GlobalActivityMenuModel
    /// Invoked with a workspace-qualified row when clicked — the app brings
    /// the window back, scopes to its Host, and reveals the session.
    private let onSelect: (GlobalActivityMenuItem) -> Void
    /// Opens the app-wide "All recent" page in this instance's window.
    private let onShowAllRecent: () -> Void
    private let statusItem: NSStatusItem
    private let popover = NSPopover()
    private var animationTimer: Timer?
    private var activityObserver: AnyCancellable?

    /// What the status button currently renders, so refreshes that change
    /// nothing skip the AppKit churn (this used to reassign image/title 8×/s
    /// unconditionally).
    private enum ButtonMode: Equatable {
        case working(blocked: Bool)
        case blocked(dark: Bool)
        case unread(dark: Bool)
        case idle

        var isWorking: Bool {
            if case .working = self { return true }
            return false
        }
    }
    private var currentMode: ButtonMode?
    /// The badged "done jobs" mark, cached per menu-bar appearance — building
    /// it parses two SVGs and composites them.
    private var badgedMarkCache: [Bool: NSImage] = [:]
    /// Orange-badged mark used when a session needs attention.
    private var blockedMarkCache: [Bool: NSImage] = [:]

    init(
        store: UnpeelStore,
        onSelect: @escaping (GlobalActivityMenuItem) -> Void,
        onShowAllRecent: @escaping () -> Void
    ) {
        self.store = store
        activityModel = store.globalActivityMenu
        self.onSelect = onSelect
        self.onShowAllRecent = onShowAllRecent
        statusItem = NSStatusBar.system.statusItem(withLength: NSStatusItem.variableLength)
        super.init()
        configureButton()
        configurePopover()
        startAnimating()
    }

    /// Short workspace tag shown next to the glyph so simultaneous instances
    /// are tellable apart; nil for the default instance, whose
    /// rendering stays exactly as it always was.
    private static let workspaceTag: String? = {
        guard let name = UnpeelWorkspaceContext.displayName else { return nil }
        return name.count > 12 ? String(name.prefix(11)) + "…" : name
    }()

    private func configureButton() {
        guard let button = statusItem.button else { return }
        button.target = self
        button.action = #selector(togglePopover)
        if let tag = Self.workspaceTag {
            button.toolTip = "Unpeel sessions — \(tag)"
        } else {
            button.toolTip = "Unpeel sessions"
        }
        // Bigger, bold braille so the spinner reads clearly in the menu bar.
        button.font = NSFont.monospacedSystemFont(ofSize: 15, weight: .bold)
    }

    private func configurePopover() {
        popover.behavior = .transient
        popover.animates = true
        let panel = MenuBarActivityPanel(
            store: store,
            activityModel: activityModel,
            onSelect: { [weak self] item in
                self?.popover.performClose(nil)
                self?.onSelect(item)
            },
            onShowAll: { [weak self] in
                self?.popover.performClose(nil)
                self?.onShowAllRecent()
            }
        )
        popover.contentViewController = NSHostingController(rootView: panel)
    }

    /// Repaints the status glyph: while any session works a timer cycles the
    /// braille spinner; blockers make that spinner orange, or show an orange-
    /// badged mark when no other session is working. Settled unread work uses
    /// a blue badge and fully idle uses the plain mark. State changes are driven
    /// by store publishes (the old version polled the store with 2-3 full
    /// tree walks at 8.3 Hz forever); the frame timer runs only while the
    /// spinner is actually visible.
    private func startAnimating() {
        activityObserver = activityModel.$activity
            .dropFirst()
            .sink { [weak self] _ in
                DispatchQueue.main.async { self?.refreshButton() }
            }
        // Menu-bar light/dark flips re-tint the non-template badged mark.
        DistributedNotificationCenter.default().addObserver(
            self,
            selector: #selector(appearanceChanged),
            name: NSNotification.Name("AppleInterfaceThemeChangedNotification"),
            object: nil
        )
        refreshButton()
    }

    @objc private func appearanceChanged() {
        Task { @MainActor in
            self.currentMode = nil
            self.refreshButton()
        }
    }

    private func updateSpinnerTimer(running: Bool) {
        if running {
            guard animationTimer == nil else { return }
            let timer = Timer.scheduledTimer(
                withTimeInterval: Theme.spinnerInterval, repeats: true
            ) { [weak self] _ in
                Task { @MainActor in self?.tickSpinner() }
            }
            timer.tolerance = Theme.spinnerInterval / 2
            // .common so it keeps ticking while the menu/popover tracks events.
            RunLoop.main.add(timer, forMode: .common)
            animationTimer = timer
        } else {
            animationTimer?.invalidate()
            animationTimer = nil
        }
    }

    /// Frame-only update while working — no store reads, no image churn.
    private func tickSpinner() {
        guard case .working(let blocked) = currentMode,
              let button = statusItem.button else { return }
        setSpinnerTitle(on: button, blocked: blocked)
    }

    private func refreshButton() {
        guard let button = statusItem.button else { return }
        // Workspace instances carry a short name label next to the glyph; the
        // default instance keeps its historical text-free rendering.
        let tag = Self.workspaceTag.map { " \($0)" } ?? ""
        let labeledPosition: NSControl.ImagePosition = tag.isEmpty ? .imageOnly : .imageLeading

        let activity = activityModel.activity
        let hasBlockers = !activity.blockers.isEmpty
        let dark = button.effectiveAppearance.bestMatch(from: [.aqua, .darkAqua]) == .darkAqua
        let mode: ButtonMode
        if !activity.jobs.isEmpty {
            mode = .working(blocked: hasBlockers)
        } else if hasBlockers {
            mode = .blocked(dark: dark)
        } else if !activity.finished.isEmpty {
            mode = .unread(dark: dark)
        } else {
            mode = .idle
        }
        updateSpinnerTimer(running: mode.isWorking)
        guard mode != currentMode else { return }
        currentMode = mode

        switch mode {
        case .working(let blocked):
            // Working: cycle the braille spinner (frames via tickSpinner).
            button.image = nil
            button.imagePosition = .noImage
            setSpinnerTitle(on: button, blocked: blocked)
        case .blocked(let dark):
            // A blocker always wins the indicator color, even if another
            // session also has unread finished work.
            button.title = tag
            button.imagePosition = labeledPosition
            if blockedMarkCache[dark] == nil {
                blockedMarkCache[dark] = AppBrand.menuBarBadgedMark(
                    foreground: dark ? .white : .black,
                    badge: NSColor(hex: 0xF59E0B)
                )
            }
            button.image = blockedMarkCache[dark]
        case .unread(let dark):
            // Settled but unread (jobs done): the Unpeel mark with a colored
            // badge. Non-template so the badge keeps its color, so we resolve
            // the mark's tint from the menu bar's own light/dark appearance.
            button.title = tag
            button.imagePosition = labeledPosition
            if badgedMarkCache[dark] == nil {
                badgedMarkCache[dark] = AppBrand.menuBarBadgedMark(
                    foreground: dark ? .white : .black,
                    badge: NSColor(srgbRed: 0x60 / 255, green: 0xA5 / 255, blue: 0xFA / 255, alpha: 1)
                )
            }
            button.image = badgedMarkCache[dark]
        case .idle:
            // Idle (no sessions running): the Unpeel brand mark, always present.
            // Template image tints itself for light and dark menu bars.
            button.title = tag
            button.imagePosition = labeledPosition
            button.image = AppBrand.menuBarMark
        }
    }

    private func currentSpinnerFrame() -> String {
        let index = Int(Date().timeIntervalSinceReferenceDate / Theme.spinnerInterval)
        return Theme.spinnerFrames[index % Theme.spinnerFrames.count]
    }

    private func setSpinnerTitle(on button: NSStatusBarButton, blocked: Bool) {
        let tag = Self.workspaceTag.map { " \($0)" } ?? ""
        let title = currentSpinnerFrame() + tag
        guard blocked else {
            button.title = title
            return
        }
        button.attributedTitle = NSAttributedString(
            string: title,
            attributes: [
                .font: NSFont.monospacedSystemFont(ofSize: 15, weight: .bold),
                .foregroundColor: NSColor(hex: 0xF59E0B),
            ]
        )
    }

    @objc private func togglePopover() {
        guard let button = statusItem.button else { return }
        if popover.isShown {
            popover.performClose(nil)
            return
        }
        // Activate BEFORE showing so the popover draws in the active state and
        // its dynamic (light/dark) colors resolve correctly — shown from the
        // background it renders inactive/mis-themed until it gets focus. Pin its
        // appearance to the app's theme override (nil = follow macOS) too.
        NSApp.activate(ignoringOtherApps: true)
        activityModel.refreshWorkspaceMetadata()
        popover.appearance = NSApp.appearance
        // Size the popover explicitly BEFORE showing: the SwiftUI content has
        // no intrinsic height on first layout, so NSHostingController
        // over-measures and NSPopover would anchor the window for a too-tall
        // height — the shrunk content then floats far below the menu bar.
        popover.contentSize = popoverContentSize()
        // The status button is a flipped view, so its visual bottom edge (where
        // the popover should hang from) is .maxY, not .minY.
        popover.show(relativeTo: button.bounds, of: button, preferredEdge: .maxY)
        // Make the popover key so it (and its content) is focused on open.
        popover.contentViewController?.view.window?.makeKey()
    }

    /// Estimated dropdown size from the live row counts (panel width 320 + 6+6
    /// padding; rows ~42pt of title/project/padding, a 9pt divider when both
    /// groups are present, else a single empty-state line).
    private func popoverContentSize() -> NSSize {
        let activity = activityModel.activity
        let rows = activity.rowCount
        let padding: CGFloat = 12
        var height: CGFloat
        if rows == 0 {
            height = padding + ActivityMenuList.emptyScrollHeight
        } else {
            let dividers = CGFloat(max(0, activity.sectionCount - 1)) * 9
            height = padding + min(CGFloat(rows) * 42 + dividers, 429)
        }
        if store.selectedHostScope == .local {
            // The persisted history page is local-only.
            height += 28
        }
        return NSSize(width: 372, height: height)
    }
}

/// Menu-bar dropdown: the shared global `ActivityMenuList`. Workspace
/// switching belongs to the app chrome, not this activity-only surface.
struct MenuBarActivityPanel: View {
    @ObservedObject var store: UnpeelStore
    @ObservedObject var activityModel: GlobalActivityMenuModel
    let onSelect: (GlobalActivityMenuItem) -> Void
    let onShowAll: () -> Void

    var body: some View {
        let activity = activityModel.activity
        ActivityMenuList(
            jobs: activity.jobs,
            blockers: activity.blockers,
            finished: activity.finished,
            onSelect: onSelect,
            onShowAll: store.selectedHostScope == .local ? onShowAll : nil
        )
        .padding(6)
        .frame(width: 360)
    }
}
