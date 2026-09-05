//
//  SessionScreenshotCapture.swift
//  UnpeelNative
//
//  User-taken screenshots for a session, launched from the gallery chip's
//  dropdown in the terminal title bar. Uses the system `screencapture` CLI
//  rather than ScreenCaptureKit: the CLI provides the native crosshair /
//  window-picker UI for free, and as a direct child process its TCC
//  screen-recording grant attributes to Unpeel.app (same discipline as
//  cua-driver). Shots land in the session's `artifacts/uploads/` — the same
//  kind phone uploads use — so they show in both galleries, and are then
//  attached to the session's prompt like a Finder drop.
//

import AppKit
import Foundation
import SwiftUI

extension Notification.Name {
    /// Session ▸ Take Screenshot… (⌘⇧S). Observed by the gallery chip of
    /// the currently displayed session, which runs the same capture-area
    /// flow as its dropdown; a no-op when no capture-eligible session is
    /// showing (same looseness as the hidden ⇧⌘R shortcut).
    static let unpeelTakeSessionScreenshot = Notification.Name(
        "unpeel.native.takeSessionScreenshot"
    )
}

enum SessionScreenshotCapture {
    enum Mode: String, CaseIterable {
        case area
        case window
        case screen

        var title: String {
            switch self {
            case .area: return "Capture area"
            case .window: return "Capture window"
            case .screen: return "Capture full screen"
            }
        }

        var symbol: String {
            switch self {
            case .area: return "rectangle.dashed"
            case .window: return "macwindow"
            case .screen: return "display"
            }
        }

        /// `screencapture` argv. `-i` is the interactive crosshair (space
        /// toggles to window picking anyway); `-W` starts interaction in
        /// window mode with `-o` dropping the drop shadow; no flags captures
        /// the whole screen immediately.
        var flags: [String] {
            switch self {
            case .area: return ["-i"]
            case .window: return ["-i", "-W", "-o"]
            case .screen: return []
            }
        }
    }

    /// Take a screenshot into the session's uploads. Interactive modes
    /// cancel cleanly (Esc writes no file → completion gets nil). On success
    /// the completion receives the saved file's URL so the caller can open
    /// the gallery on it. Without the Screen Recording grant this raises the
    /// system prompt instead of capturing — the grant needs an app relaunch
    /// to apply, so there is nothing useful to do in-line.
    @MainActor
    static func capture(
        _ mode: Mode,
        sessionID: String,
        completion: @escaping @MainActor (URL?) -> Void
    ) {
        guard ComputerPermissions.screenRecordingGranted() else {
            ComputerPermissions.request("Screen Recording")
            completion(nil)
            return
        }
        guard let dir = SessionArtifactStore.kindDir(sessionID, kind: "uploads") else {
            completion(nil)
            return
        }
        try? FileManager.default.createDirectory(at: dir, withIntermediateDirectories: true)
        let formatter = DateFormatter()
        formatter.dateFormat = "yyyy-MM-dd-HHmmss"
        let url = dir.appendingPathComponent("screenshot-\(formatter.string(from: Date())).png")

        // Full-screen fires immediately — give the dropdown's fade-out a
        // beat so the menu isn't in the shot.
        let delay: DispatchTimeInterval = mode == .screen ? .milliseconds(400) : .milliseconds(0)
        DispatchQueue.main.asyncAfter(deadline: .now() + delay) {
            run(mode, url: url, completion: completion)
        }
    }

    @MainActor
    private static func run(
        _ mode: Mode,
        url: URL,
        completion: @escaping @MainActor (URL?) -> Void
    ) {
        let process = Process()
        process.executableURL = URL(fileURLWithPath: "/usr/sbin/screencapture")
        process.arguments = mode.flags + [url.path]
        process.terminationHandler = { _ in
            Task { @MainActor in
                completion(FileManager.default.fileExists(atPath: url.path) ? url : nil)
            }
        }
        do {
            try process.run()
        } catch {
            NSLog("[UnpeelNative] screencapture launch failed: \(error)")
            completion(nil)
        }
    }
}

/// Owns the AppKit dropdown for the gallery chip's screenshot menu — the
/// same hand-positioned NSMenu pattern as WorkspaceMenuController, so the
/// popup right-aligns to the chevron instead of SwiftUI Menu's forced
/// left-align.
@MainActor
final class SessionScreenshotMenuController: NSObject {
    var onCapture: ((SessionScreenshotCapture.Mode) -> Void)?
    weak var anchorView: NSView?

    func present() {
        guard let anchorView else { return }

        let menu = NSMenu()
        menu.autoenablesItems = false
        for mode in SessionScreenshotCapture.Mode.allCases {
            let item = NSMenuItem(
                title: mode.title,
                action: #selector(handleSelection(_:)),
                keyEquivalent: ""
            )
            item.target = self
            item.representedObject = mode.rawValue
            if let icon = NSImage(
                systemSymbolName: mode.symbol,
                accessibilityDescription: mode.title
            ) {
                item.image = icon
            }
            menu.addItem(item)
        }

        let origin = NSPoint(x: anchorView.bounds.maxX - menu.size.width, y: -4)
        menu.popUp(positioning: nil, at: origin, in: anchorView)
    }

    @objc private func handleSelection(_ sender: NSMenuItem) {
        guard
            let raw = sender.representedObject as? String,
            let mode = SessionScreenshotCapture.Mode(rawValue: raw)
        else { return }
        onCapture?(mode)
    }
}

/// Invisible AppKit view tracking the chevron's frame so the menu can be
/// positioned relative to it (twin of WorkspaceMenuAnchor).
struct SessionScreenshotMenuAnchor: NSViewRepresentable {
    let controller: SessionScreenshotMenuController

    func makeNSView(context: Context) -> NSView {
        let view = NSView()
        controller.anchorView = view
        return view
    }

    func updateNSView(_ nsView: NSView, context: Context) {
        controller.anchorView = nsView
    }
}
