//
//  Chrome.swift
//  UnpeelNative
//
//  Window-chrome building blocks: vibrancy backgrounds, drag regions,
//  the custom 38px titlebar (DESIGN.md §1).
//

import AppKit
import SwiftUI

// MARK: - Vibrancy

/// NSVisualEffectView wrapper. `.sidebar` behind the sidebar,
/// `.underWindowBackground` behind the content area (DESIGN.md §1).
struct VisualEffectBackground: NSViewRepresentable {
    let material: NSVisualEffectView.Material
    /// `.behindWindow` for window chrome; `.withinWindow` for in-window
    /// overlays (⌘K palette, ⌃Tab switcher) that should frost the app
    /// content beneath them rather than the desktop.
    var blendingMode: NSVisualEffectView.BlendingMode = .behindWindow
    /// `.followsWindowActiveState` (chrome dims with the window, the shipped
    /// default) or `.active` for the transparency glass layers — with
    /// follows, every deactivation drops the behind-window blur and the
    /// whole glass look collapses to flat gray.
    var state: NSVisualEffectView.State = .followsWindowActiveState

    func makeNSView(context _: Context) -> NSVisualEffectView {
        let view = NSVisualEffectView()
        view.material = material
        view.blendingMode = blendingMode
        view.state = state
        return view
    }

    func updateNSView(_ view: NSVisualEffectView, context _: Context) {
        view.material = material
        view.blendingMode = blendingMode
        view.state = state
    }
}

/// Sidebar background: native sidebar vibrancy plus a subdued neutral wash.
/// The top/bottom row fade is owned by `SidebarListFadeMask`.
///
/// This is the Appearance "Frame" surface: one window-spanning backdrop
/// behind the sidebar, the corner strip at the sidebar/content boundary,
/// and everything a translucent terminal reveals — one surface, so the
/// content pane's rounded corners can never expose a differently-shaded
/// band. Frame opacity is an honest linear scale (2026-09-01): 100% is the
/// opaque design color and every other value is a plain wash at exactly
/// that alpha over the hudWindow glass base. The old 90% detent that
/// rendered the system `.sidebar` material — with its own AppKit-controlled
/// translucency, which read as a different KIND of transparency mid-scale —
/// is removed.
struct SidebarBackground: View {
    var body: some View {
        FrameBackdrop()
            .ignoresSafeArea()
    }
}

/// The frame material itself, safe-area-respecting so it can also paint
/// in-window regions: the gaps between split terminal pane cards use this
/// exact stack, so a pane group's gutters match the window frame around the
/// content surface.
struct FrameBackdrop: View {
    @ObservedObject private var tint = AppTintModel.shared
    @ObservedObject private var transparency = TransparencyModel.shared

    var body: some View {
        ZStack {
            // Translucent paths stack their wash over a native glass base.
            // .hudWindow is the clearest standard material, so low opacities
            // read as glass rather than a gray slab. WITHIN-window blending,
            // never behind-window: a behind-window blur is re-sampled from
            // the desktop by WindowServer on the GPU every time anything in
            // the window changes, and a TUI redrawing at 60 fps (OpenCode's
            // progress UI) turned that into 80-90% whole-GPU utilization on
            // a Retina/ProMotion display (unpeel#9). Within-window frosts
            // only the window's own content, which costs nothing while the
            // terminal repaints.
            if !transparency.backgroundUsesDesignTone {
                // Custom tone: flat color (dark appearance; light keeps its
                // designed white) plus the workspace wash every area shares.
                // The per-area neutral tints are skipped so an equal Surface
                // tone renders the identical color.
                if transparency.backgroundOpacity < 1 {
                    VisualEffectBackground(
                        material: .hudWindow,
                        blendingMode: .withinWindow,
                        state: .active
                    )
                }
                Theme.toneColor(transparency.backgroundTone)
                    .opacity(min(transparency.backgroundOpacity, 1))
                Theme.workspaceGlassWash
            } else if transparency.backgroundOpacity >= 1 {
                Theme.appBackground
                Theme.sidebarTint
            } else {
                VisualEffectBackground(
                    material: .hudWindow,
                    blendingMode: .withinWindow,
                    state: .active
                )
                Theme.appBackground.opacity(transparency.backgroundOpacity)
                Theme.sidebarTint
            }
        }
    }
}

/// Content-region backdrop behind terminals and pages.
///
/// Opaque Surfaces: the same canvas paint everything above it uses, so any
/// bleed (rounded-corner antialiasing, transient gaps) matches. Translucent
/// Surfaces: fully clear — the window-spanning frame backdrop
/// (SidebarBackground) is what shows behind the translucent Ghostty
/// surface, and any extra wash here would shade the content region
/// differently from the corner strip, making the rounded leading corners
/// read as a band again.
struct ContentBackground: View {
    @ObservedObject private var transparency = TransparencyModel.shared

    var body: some View {
        // Deliberately NOT ignoresSafeArea: the content pane is inset from
        // the window top, and a safe-area-ignoring backdrop whose top edge
        // sits inside the titlebar safe-area region gets extended all the
        // way to the window top — a square slab behind the surface.
        ZStack {
            if transparency.surfaceOpacity < 1 {
                Color.clear
            } else {
                SurfaceBackdrop()
            }
        }
    }
}

/// THE one content-surface paint: the terminal canvas color (Surface tone,
/// workspace tint, and light/dark aware via `Theme.terminalBackgroundNSColor`)
/// at the Surface opacity. Settings and every non-terminal page use this so
/// the main screen reads as ONE background — the same color and alpha the
/// Ghostty surface paints for the terminal itself.
struct SurfaceBackdrop: View {
    @ObservedObject private var tint = AppTintModel.shared
    @ObservedObject private var transparency = TransparencyModel.shared

    var body: some View {
        Color(nsColor: Theme.terminalBackgroundNSColor)
            .opacity(transparency.surfaceOpacity)
    }
}

// MARK: - Drag region

/// Transparent strip that moves the window when dragged and toggles
/// maximize on double-click (the `-webkit-app-region: drag` equivalent).
///
/// The main window sets `isMovable = false` — the system's own titlebar-band
/// drag ran IN PARALLEL with SwiftUI gestures under the pane headers, and no
/// view-level opt-out reaches that server-side machinery. That flag also
/// disables `performDrag(with:)`, so this view drags the frame by hand:
/// programmatic moves are always allowed.
struct WindowDragArea: NSViewRepresentable {
    final class DragView: NSView {
        override func mouseDown(with event: NSEvent) {
            guard let window else { return }
            if event.clickCount == 2 {
                window.zoom(nil)
                return
            }
            let startMouse = NSEvent.mouseLocation
            let startOrigin = window.frame.origin
            while true {
                guard let next = window.nextEvent(
                    matching: [.leftMouseDragged, .leftMouseUp]
                ) else { break }
                if next.type == .leftMouseUp { break }
                let now = NSEvent.mouseLocation
                window.setFrameOrigin(NSPoint(
                    x: startOrigin.x + (now.x - startMouse.x),
                    y: startOrigin.y + (now.y - startMouse.y)
                ))
            }
        }
    }

    func makeNSView(context _: Context) -> DragView { DragView() }
    func updateNSView(_: DragView, context _: Context) {}
}

// MARK: - Titlebar

/// Custom 38px titlebar over the content pane. Its centered breadcrumb is the
/// selected project, with any nested project/group segments after it. Git
/// branch keeps its compact historical icon suffix. Background = terminal
/// background while a terminal shows.
struct TitleBarView: View {
    let segments: [String]
    var branch: String? = nil
    var branchIsWorktree = false
    /// Strip height — the library pages keep the classic 38pt; the
    /// workspace breadcrumb strip is compact so the panes sit higher.
    var height: CGFloat = Theme.titlebarHeight
    /// Vertical nudge for the centered title (the compact strip rides it
    /// up a touch to align with the traffic lights).
    var titleYOffset: CGFloat = 0

    // No background fill: with panes as rounded cards the strip floats on
    // the window backdrop, collapsed sidebar included.
    var body: some View {
        ZStack {
            WindowDragArea()
            titleText
                .offset(y: titleYOffset)
        }
        .frame(maxWidth: .infinity)
        .frame(height: height)
    }

    private var titleText: some View {
        HStack(spacing: 5) {
            ForEach(Array(segments.enumerated()), id: \.offset) { index, segment in
                if index > 0 {
                    titleSeparator
                }
                Text(segment)
                    .font(.system(size: 13, weight: index > 0 ? .medium : .semibold))
                    .foregroundStyle(Theme.mutedForeground)
                    .lineLimit(1)
            }
            // Keep the historical compact branch suffix: icon + smaller,
            // reduced-opacity name without another breadcrumb separator.
            // The value is resolved asynchronously and cached by the store.
            if let branch, !branch.isEmpty {
                HStack(spacing: 3) {
                    ChromeIconView(icon: branchIsWorktree ? .branch : .gitBranch, size: 12)
                    Text(branch)
                        .font(.system(size: 12, weight: .medium))
                        .lineLimit(1)
                }
                .foregroundStyle(Theme.mutedForeground)
                .opacity(0.55)
                .padding(.leading, 2)
            }
        }
    }

    private var titleSeparator: some View {
        Text("›")
            .font(.system(size: 13, weight: .semibold))
            .foregroundStyle(Theme.mutedForeground.opacity(0.55))
            .allowsHitTesting(false)
    }
}
