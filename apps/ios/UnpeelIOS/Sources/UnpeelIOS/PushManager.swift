//
//  PushManager.swift
//  UnpeelIOS
//
//  APNs registration + the app-side glue for remote notifications. The Mac
//  (via the relay, which holds the APNs key) pushes needs-input, finished, and
//  informational App alerts; this captures the token, hands it to the connection layer to
//  register with the Mac, and routes a tapped notification to its session.
//

import Foundation
#if os(iOS)
import UIKit
import UserNotifications
#endif

enum PushRegistrationState: Equatable {
    case notRequested
    case requestingPermission
    case permissionDenied
    case registering
    case registered(environment: String)
    case failed(String)

    var diagnosticLabel: String {
        switch self {
        case .notRequested:
            return "Not requested"
        case .requestingPermission:
            return "Waiting for notification permission…"
        case .permissionDenied:
            return "Notifications are denied in iOS Settings"
        case .registering:
            return "Waiting for an APNs device token…"
        case .registered(let environment):
            return environment == "production" ? "Ready (production)" : "Ready (sandbox)"
        case .failed(let message):
            return "Registration failed: \(message)"
        }
    }

    var permissionWasDenied: Bool {
        if case .permissionDenied = self { return true }
        return false
    }

    /// Broken-delivery states worth surfacing outside the settings sheet
    /// (the sidebar warning). Transient startup states stay quiet so a
    /// healthy launch never flashes a warning while registration settles.
    var sidebarWarning: String? {
        switch self {
        case .permissionDenied:
            return "Notifications are off"
        case .failed:
            return "Notifications aren't working"
        case .notRequested, .requestingPermission, .registering, .registered:
            return nil
        }
    }

    var canRetry: Bool {
        switch self {
        case .notRequested, .permissionDenied, .failed:
            return true
        case .requestingPermission, .registering, .registered:
            return false
        }
    }
}

@MainActor
final class PushManager: NSObject, ObservableObject {
    static let shared = PushManager()

    /// Hex APNs device token, once registration succeeds.
    private(set) var tokenHex: String?

    /// User-visible diagnostics for the device half of push delivery. Never
    /// includes the token itself.
    @Published private(set) var registrationState: PushRegistrationState = .notRequested

    /// APNs environment this build targets. Debug/dev builds provision the
    /// sandbox APNs; release builds the production host. The Mac forwards this
    /// to the relay so it hits the matching APNs host.
    let environment: String = {
        #if DEBUG
        return "sandbox"
        #else
        return "production"
        #endif
    }()

    /// Called with (tokenHex, environment) when a token first arrives or
    /// changes. The connection layer uploads it to the paired Mac.
    var onTokenChange: ((String, String) -> Void)?

    /// A tapped notification's session id — the root view selects it.
    var onOpenSession: ((String) -> Void)?

    /// Request authorization and, if granted, register for remote
    /// notifications. Safe to call repeatedly.
    func requestAndRegister() {
        #if os(iOS)
        registrationState = .requestingPermission
        let center = UNUserNotificationCenter.current()
        center.requestAuthorization(options: [.alert, .sound, .badge]) { @Sendable granted, error in
            let failure = error?.localizedDescription
            Task { @MainActor in
                if let failure {
                    self.registrationState = .failed(failure)
                } else if granted {
                    self.registrationState = .registering
                    UIApplication.shared.registerForRemoteNotifications()
                } else {
                    self.registrationState = .permissionDenied
                }
            }
        }
        #endif
    }

    /// APNs delivered a device token. Hex-encode, cache, and hand it off.
    func didRegister(deviceToken: Data) {
        let hex = deviceToken.map { String(format: "%02x", $0) }.joined()
        registrationState = .registered(environment: environment)
        guard hex != tokenHex else { return }
        tokenHex = hex
        onTokenChange?(hex, environment)
    }

    func didFailToRegister(message: String) {
        registrationState = .failed(message)
    }

    /// Re-hand the cached token (e.g. after (re)pairing so the new Mac client
    /// gets it). No-op until a token exists.
    func uploadCachedToken() {
        guard let tokenHex else { return }
        onTokenChange?(tokenHex, environment)
    }
}

#if os(iOS)
/// Minimal UIKit app delegate: SwiftUI's `App` can't receive the remote-
/// notification callbacks, so an adaptor bridges them into `PushManager`.
/// `@MainActor` because `UIApplicationDelegate` /
/// `UNUserNotificationCenterDelegate` are main-actor isolated in the SDK.
@MainActor
final class PushAppDelegate: NSObject, UIApplicationDelegate, UNUserNotificationCenterDelegate {
    func application(
        _: UIApplication,
        didFinishLaunchingWithOptions _: [UIApplication.LaunchOptionsKey: Any]? = nil
    ) -> Bool {
        UNUserNotificationCenter.current().delegate = self
        PushManager.shared.requestAndRegister()
        return true
    }

    func application(
        _: UIApplication,
        didRegisterForRemoteNotificationsWithDeviceToken deviceToken: Data
    ) {
        PushManager.shared.didRegister(deviceToken: deviceToken)
    }

    func application(
        _: UIApplication,
        didFailToRegisterForRemoteNotificationsWithError error: Error
    ) {
        PushManager.shared.didFailToRegister(message: error.localizedDescription)
        NSLog("[push] APNs registration failed: \(error.localizedDescription)")
    }

    // The UNUserNotificationCenterDelegate requirements are nonisolated in the
    // SDK; keep them so (the @MainActor class would otherwise "cross" isolation)
    // and hop to the main actor only for the store call.

    // Foreground: still surface the banner (you may be on another session).
    nonisolated func userNotificationCenter(
        _: UNUserNotificationCenter,
        willPresent _: UNNotification,
        withCompletionHandler completionHandler: @escaping (UNNotificationPresentationOptions) -> Void
    ) {
        completionHandler([.banner, .sound])
    }

    // Tap → open the session named in the payload.
    nonisolated func userNotificationCenter(
        _: UNUserNotificationCenter,
        didReceive response: UNNotificationResponse,
        withCompletionHandler completionHandler: @escaping () -> Void
    ) {
        let sessionID = response.notification.request.content.userInfo["sessionId"] as? String
        completionHandler()
        if let sessionID {
            Task { @MainActor in PushManager.shared.onOpenSession?(sessionID) }
        }
    }
}
#endif
