//
//  ViewerAvatarsView.swift
//  UnpeelNative
//
//  Small avatar chips for devices currently viewing a session's terminal
//  (fed by ViewerPresenceStore). Mounted at the trailing edge of the
//  pane header in TerminalPaneContainer, alongside the shared-grid fit control.
//

import AppKit
import SwiftUI

/// One header surface for device observation and the Host's explicit fit.
/// The fit marker has no device identity: do not attribute it to an arbitrary
/// viewer, or synthesize a live viewer from a marker that survives disconnects.
struct TerminalPresenceView: View {
    @ObservedObject private var presence = ViewerPresenceStore.shared
    let sessionID: String
    let showsLocalViewers: Bool
    let fittedGrid: PhoneResizeOverride?
    let onFitToDesktop: () -> Void

    var body: some View {
        // The disk feed belongs to this Controller's own Host. Session ids
        // can collide across Hosts; remote presence needs a Host projection.
        let viewers = showsLocalViewers ? presence.viewers[sessionID] ?? [] : []
        HStack(spacing: 4) {
            if !viewers.isEmpty {
                ViewerAvatarsView(viewers: viewers)
            }
            if let fittedGrid {
                PaneFitToDesktopButton(grid: fittedGrid, onRevert: onFitToDesktop)
            }
        }
        .fixedSize()
    }
}

struct ViewerAvatarsView: View {
    let viewers: [ViewerInfo]

    private static let maxChips = 4
    private static let chipSize: CGFloat = 20

    var body: some View {
        HStack(spacing: -5) {
            ForEach(viewers.prefix(Self.maxChips)) { viewer in
                ViewerAvatarChip(viewer: viewer, size: Self.chipSize)
            }
            if viewers.count > Self.maxChips {
                overflowChip(count: viewers.count - Self.maxChips)
            }
        }
        .accessibilityElement(children: .combine)
        .accessibilityLabel("\(viewers.count) viewing device\(viewers.count == 1 ? "" : "s")")
        .accessibilityValue(viewers.map(\.displayName).joined(separator: ", "))
    }

    private func overflowChip(count: Int) -> some View {
        Text("+\(count)")
            .font(.system(size: 9, weight: .semibold))
            .foregroundStyle(.secondary)
            .frame(width: Self.chipSize, height: Self.chipSize)
            .background(Circle().fill(Color(nsColor: .quaternaryLabelColor)))
            .overlay(Circle().strokeBorder(chipBorderColor, lineWidth: 1))
            .help(
                viewers.dropFirst(Self.maxChips)
                    .map(\.displayName)
                    .joined(separator: ", ")
            )
    }
}

private struct ViewerAvatarChip: View {
    let viewer: ViewerInfo
    let size: CGFloat

    var body: some View {
        Text(initials)
            .font(.system(size: 8, weight: .semibold))
            .foregroundStyle(.white)
            .frame(width: size, height: size)
            .background(Circle().fill(chipColor))
            .overlay(Circle().strokeBorder(chipBorderColor, lineWidth: 1))
            .help("\(viewer.displayName) — viewing this terminal")
            .accessibilityLabel("\(viewer.displayName), viewing this terminal")
    }

    private var initials: String {
        let words = viewer.displayName
            .split(whereSeparator: { $0 == " " || $0 == "-" || $0 == "_" })
            .prefix(2)
        let letters = words.compactMap(\.first)
        if letters.isEmpty { return "?" }
        return String(letters).uppercased()
    }

    /// Stable per-device hue so a device keeps its color across refreshes and
    /// launches (Hasher is seeded per-process, so avoid hashValue here).
    private var chipColor: Color {
        var hash: UInt32 = 2_166_136_261
        for byte in viewer.id.utf8 {
            hash = (hash ^ UInt32(byte)) &* 16_777_619
        }
        let hue = Double(hash % 360) / 360
        return Color(hue: hue, saturation: 0.55, brightness: 0.72)
    }
}

/// Hairline that separates overlapping chips from each other and the bar.
private var chipBorderColor: Color {
    Color(nsColor: .windowBackgroundColor).opacity(0.9)
}
