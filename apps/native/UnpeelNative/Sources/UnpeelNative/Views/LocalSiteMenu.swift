//
//  LocalSiteMenu.swift
//  UnpeelNative
//
//  Local sites a project's sessions are serving (host-probed live loopback
//  URLs from `detected_local_urls`). Surfaced on the sidebar project row: a
//  globe button next to the project name opens the site (or drops the full
//  AppKit URL/Stop-server menu when several are live), and the project's
//  context menu lists the same links.
//

import AppKit
import SwiftUI

enum LocalSiteMenu {
    static func open(_ url: String) {
        guard let parsed = URL(string: url),
              parsed.scheme == "http" || parsed.scheme == "https"
        else { return }
        NSWorkspace.shared.open(parsed)
    }

    /// "http://localhost:5173/foo" → "localhost:5173" for compact labels.
    static func compactLabel(_ url: String) -> String {
        String(
            url
                .replacingOccurrences(of: "https://", with: "")
                .replacingOccurrences(of: "http://", with: "")
                .prefix(while: { $0 != "/" })
        )
    }
}

/// Ghost split control for the window-chrome title strip: the globe opens
/// the (first) live site, the chevron drops the hand-positioned AppKit menu
/// (full URLs + session-owned Stop rows). Styled like the titlebar's other
/// icon buttons — nothing drawn at rest, hover highlight per half.
struct LocalSiteChip: View {
    let urls: [String]

    @State private var controller = LocalSiteMenuController()
    @State private var leftHovering = false
    @State private var rightHovering = false

    var body: some View {
        HStack(spacing: 0) {
            Button {
                if urls.count == 1 {
                    LocalSiteMenu.open(urls.first ?? "")
                } else {
                    controller.urls = urls
                    controller.present()
                }
            } label: {
                ChromeIconView(icon: .globe, size: 14)
                    .foregroundStyle(leftHovering ? Theme.foreground : Theme.mutedForeground)
                    .frame(width: 32, height: 26)
                    .background(leftHovering ? Theme.hoverRow : .clear)
                    .contentShape(Rectangle())
            }
            .buttonStyle(.plain)
            .onHover { leftHovering = $0 }
            .help(
                urls.count == 1
                    ? "Open \(urls.first ?? "") in browser"
                    : "Local sites (\(urls.count))"
            )
            .accessibilityLabel("Open local site")

            Rectangle()
                .fill(Theme.foreground.opacity(0.10))
                .frame(width: 1, height: 14)
                .allowsHitTesting(false)

            Button {
                controller.urls = urls
                controller.present()
            } label: {
                Image(systemName: "chevron.down")
                    .font(.system(size: 9, weight: .semibold))
                    .foregroundStyle(rightHovering ? Theme.foreground : Theme.mutedForeground)
                    .frame(width: 25, height: 26)
                    .background(rightHovering ? Theme.hoverRow : .clear)
                    .contentShape(Rectangle())
            }
            .buttonStyle(.plain)
            .background(LocalSiteMenuAnchor(controller: controller))
            .onHover { rightHovering = $0 }
            .help("Choose local site")
            .accessibilityLabel("Choose local site")
        }
        .frame(height: 26)
        .clipShape(RoundedRectangle(cornerRadius: 6, style: .continuous))
        .contentShape(RoundedRectangle(cornerRadius: 6, style: .continuous))
        // Ghost control, like the sidebar toggle and menu buttons on the
        // left of the strip: no fill or border at rest, each half lights up
        // on hover only.
        .animation(.easeInOut(duration: 0.12), value: leftHovering)
        .animation(.easeInOut(duration: 0.12), value: rightHovering)
        .fixedSize()
    }
}

/// Globe button beside a project's name while the project family serves at
/// least one live local site. One URL opens directly; several present the
/// hand-positioned AppKit dropdown (full URLs + session-owned Stop rows).
struct ProjectLinkButton: View {
    let urls: [String]

    @State private var controller = LocalSiteMenuController()
    @State private var hovering = false

    var body: some View {
        Button {
            if urls.count == 1 {
                LocalSiteMenu.open(urls.first ?? "")
            } else {
                controller.urls = urls
                controller.present()
            }
        } label: {
            ChromeIconView(icon: .globe, size: 12)
                .foregroundStyle(hovering ? Theme.foreground : Theme.mutedForeground)
                .frame(width: 18, height: 18)
                .contentShape(Rectangle())
        }
        .buttonStyle(.plain)
        .background(LocalSiteMenuAnchor(controller: controller))
        .onHover { hovering = $0 }
        .help(
            urls.count == 1
                ? "Open \(urls.first ?? "") in browser"
                : "Local sites (\(urls.count))"
        )
        .accessibilityLabel("Open local site")
        .animation(.easeInOut(duration: 0.12), value: hovering)
    }
}

@MainActor
final class LocalSiteMenuController: NSObject {
    var urls: [String] = []
    weak var anchorView: NSView?

    /// Resolve each URL's serving process off-main (lsof + ancestry walk,
    /// ~100ms, only on dropdown open — never on a timer), then present:
    /// open rows for every URL, and Stop rows for the session-owned servers.
    func present() {
        let urls = self.urls
        DispatchQueue.global(qos: .userInitiated).async {
            let servers: [(url: String, label: String)] = urls.compactMap { url in
                resolveServer(url).map { (url, $0) }
            }
            DispatchQueue.main.async { [weak self] in
                self?.presentMenu(urls: urls, stoppable: servers)
            }
        }
    }

    private func presentMenu(urls: [String], stoppable: [(url: String, label: String)]) {
        guard let anchorView else { return }

        let menu = NSMenu()
        menu.autoenablesItems = false
        for url in urls {
            let item = NSMenuItem(
                title: url,
                action: #selector(handleSelection(_:)),
                keyEquivalent: ""
            )
            item.target = self
            item.representedObject = url
            // Template image, so the menu tints it like a system symbol.
            item.image = ChromeIconStore.image(for: .globe)
            menu.addItem(item)
        }
        if !stoppable.isEmpty {
            menu.addItem(.separator())
            for server in stoppable {
                let item = NSMenuItem(
                    title: "Stop \(server.label)",
                    action: #selector(handleStop(_:)),
                    keyEquivalent: ""
                )
                item.target = self
                item.representedObject = server.url
                item.image = NSImage(
                    systemSymbolName: "stop.circle",
                    accessibilityDescription: nil
                )
                menu.addItem(item)
            }
        }

        let origin = NSPoint(x: anchorView.bounds.maxX - menu.size.width, y: -4)
        menu.popUp(positioning: nil, at: origin, in: anchorView)
    }

    @objc private func handleSelection(_ sender: NSMenuItem) {
        guard let url = sender.representedObject as? String else { return }
        LocalSiteMenu.open(url)
    }

    @objc private func handleStop(_ sender: NSMenuItem) {
        guard let url = sender.representedObject as? String else { return }
        DispatchQueue.global(qos: .userInitiated).async {
            let stopped = runHostVerb(["__stop_local_site_server__", url]) != nil
            DispatchQueue.main.async {
                ToastCenter.shared.show(
                    stopped ? "Server stopped" : "Couldn't stop server",
                    systemImage: stopped ? "stop.circle" : "exclamationmark.triangle"
                )
            }
        }
    }
}

/// "vite-style" menu label for a session-owned server: `localhost:5173
/// (node)`. Nil when nothing listens or the server is not session-owned —
/// Unpeel never offers to kill infrastructure it didn't start.
private func resolveServer(_ url: String) -> String? {
    guard let line = runHostVerb(["__local_site_server__", url]) else { return nil }
    let parts = line.split(separator: "\t").map(String.init)
    guard parts.count == 3, parts[2] != "-" else { return nil }
    let compact = url
        .replacingOccurrences(of: "https://", with: "")
        .replacingOccurrences(of: "http://", with: "")
        .prefix(while: { $0 != "/" })
    return "\(compact) (\(parts[1]))"
}

/// Run an `unpeel-host` verb and return trimmed stdout on success.
private func runHostVerb(_ arguments: [String]) -> String? {
    let process = Process()
    process.executableURL = URL(fileURLWithPath: LaunchConfig.hostBinary)
    process.arguments = arguments
    let pipe = Pipe()
    process.standardOutput = pipe
    process.standardError = FileHandle.nullDevice
    guard (try? process.run()) != nil else { return nil }
    let data = pipe.fileHandleForReading.readDataToEndOfFile()
    process.waitUntilExit()
    guard process.terminationStatus == 0 else { return nil }
    return String(data: data, encoding: .utf8)?
        .trimmingCharacters(in: .whitespacesAndNewlines)
}

private struct LocalSiteMenuAnchor: NSViewRepresentable {
    let controller: LocalSiteMenuController

    func makeNSView(context: Context) -> NSView {
        let view = NSView()
        controller.anchorView = view
        return view
    }

    func updateNSView(_ nsView: NSView, context: Context) {
        controller.anchorView = nsView
    }
}

