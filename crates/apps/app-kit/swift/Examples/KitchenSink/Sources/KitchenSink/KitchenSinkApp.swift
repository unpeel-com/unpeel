import AppKit
import GhosttyTerminal
import SwiftUI

@MainActor
private final class KitchenSinkApplicationDelegate: NSObject, NSApplicationDelegate {
    func applicationWillFinishLaunching(_ notification: Notification) {
        NSApplication.shared.setActivationPolicy(.regular)
        if ProcessInfo.processInfo.environment["UNPEEL_KITCHEN_GHOSTTY_DEBUG"] == "1" {
            TerminalDebugLog.enable([.lifecycle, .metrics])
            TerminalDebugLog.sink = { message in
                try? FileHandle.standardError.write(contentsOf: Data("\(message)\n".utf8))
            }
        }
    }

    func applicationDidFinishLaunching(_ notification: Notification) {
        NSApplication.shared.activate(ignoringOtherApps: true)
    }
}

@main
struct KitchenSinkApp: App {
    @NSApplicationDelegateAdaptor(KitchenSinkApplicationDelegate.self)
    private var applicationDelegate
    @StateObject private var host = MiniHost()

    var body: some Scene {
        WindowGroup("App Kit Kitchen Sink") {
            ContentView(host: host)
                .frame(minWidth: 1_200, minHeight: 700)
                .onAppear {
                    NSApplication.shared.setActivationPolicy(.regular)
                    NSApplication.shared.activate(ignoringOtherApps: true)
                }
                .onReceive(NotificationCenter.default.publisher(
                    for: NSApplication.willTerminateNotification
                )) { _ in
                    host.shutdown()
                }
        }
        .defaultSize(width: 1_560, height: 860)
    }
}
