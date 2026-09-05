import SwiftUI

@main
struct UnpeelIOSApp: App {
    // Bridges the remote-notification callbacks (which SwiftUI's App can't
    // receive) into PushManager.
    @UIApplicationDelegateAdaptor(PushAppDelegate.self) private var pushDelegate

    var body: some Scene {
        WindowGroup {
            UnpeelIOSRootView()
        }
    }
}
