//
//  ViewerAvatarsView.swift
//  UnpeelNative
//
//  Small avatar chips for devices currently viewing a session's terminal
//  (fed by ViewerPresenceStore). Mounted at the trailing edge of the
//  terminal title bar in TerminalArea.
//

import AppKit
import OpenDirectory
import SwiftUI

/// Loads the local macOS user's account picture without any permission
/// prompt, via OpenDirectory's local node. Used as the avatar for a viewer
/// whose paired device identity is this Mac itself (and, later, remote-Mac
/// viewers matching the local host).
@MainActor
enum MacUserAvatar {
    private static var cached: NSImage??

    static func current() -> NSImage? {
        if let cached { return cached }
        let image = load()
        cached = .some(image)
        return image
    }

    private static func load() -> NSImage? {
        do {
            let session = ODSession.default()
            let node = try ODNode(session: session, name: "/Local/Default")
            let record = try node.record(
                withRecordType: kODRecordTypeUsers,
                name: NSUserName(),
                attributes: [kODAttributeTypeJPEGPhoto]
            )
            let values = try record.values(forAttribute: kODAttributeTypeJPEGPhoto)
            for value in values {
                if let data = value as? Data, let image = NSImage(data: data) {
                    return image
                }
            }
        } catch {
            // No local record / no picture — callers fall back to initials.
        }
        return nil
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
        .accessibilityLabel("\(viewers.count) viewer\(viewers.count == 1 ? "" : "s")")
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
        Group {
            if let image = macUserImage {
                Image(nsImage: image)
                    .resizable()
                    .aspectRatio(contentMode: .fill)
                    .frame(width: size, height: size)
                    .clipShape(Circle())
            } else {
                Text(initials)
                    .font(.system(size: 8, weight: .semibold))
                    .foregroundStyle(.white)
                    .frame(width: size, height: size)
                    .background(Circle().fill(chipColor))
            }
        }
        .overlay(Circle().strokeBorder(chipBorderColor, lineWidth: 1))
        .help("\(viewer.displayName) — \(kindDescription)")
    }

    /// This Mac's own paired identity gets the local account picture; every
    /// other viewer falls back to colored initials.
    private var macUserImage: NSImage? {
        guard viewer.displayName == Host.current().localizedName else { return nil }
        return MacUserAvatar.current()
    }

    private var kindDescription: String {
        switch viewer.kind {
        case .mobile: return "Viewing on iPhone"
        case .remote: return "Viewing remotely"
        }
    }

    private var initials: String {
        let words = viewer.displayName
            .split(whereSeparator: { $0 == " " || $0 == "-" || $0 == "_" })
            .prefix(2)
        let letters = words.compactMap(\.first)
        if letters.isEmpty { return "?" }
        return String(letters).uppercased()
    }

    /// Stable per-name hue so a device keeps its color across refreshes and
    /// launches (Hasher is seeded per-process, so avoid hashValue here).
    private var chipColor: Color {
        var hash: UInt32 = 2_166_136_261
        for byte in viewer.displayName.utf8 {
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
