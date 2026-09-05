//
//  DesktopNotifier.swift
//  UnpeelNative
//
//  macOS Notification Center banners for the triggers that drive phone push:
//  needs input, opted-in completion, and informational App alerts. The two
//  channels are independent — the Mac notifies when the
//  desktop isn't already showing the session, the phone push fires when no
//  phone is viewing it — so you're told on whichever device you're at.
//

import Combine
import Foundation
import UserNotifications

enum DesktopNotificationTestDiagnostic: Equatable {
    case idle
    case checking
    case sent
    case denied
    case alertsDisabled
    case failed(String)

    var label: String {
        switch self {
        case .idle: "Not run"
        case .checking: "Sending…"
        case .sent: "Sent to macOS"
        case .denied: "Notifications are off"
        case .alertsDisabled: "Banners are off"
        case .failed(let message): "Failed — \(message)"
        }
    }

    var isChecking: Bool {
        self == .checking
    }

    var needsSystemSettings: Bool {
        self == .denied || self == .alertsDisabled
    }
}

@MainActor
final class DesktopNotifier: NSObject, ObservableObject, UNUserNotificationCenterDelegate {
    static let shared = DesktopNotifier()

    /// Tapping a banner selects that session. Wired by `UnpeelStore`.
    var onSelectSession: ((String) -> Void)?

    /// Tapping a BACKGROUND-workspace attention banner (workspace pool)
    /// rescopes the window to that workspace and selects the session. Wired
    /// by the app delegate alongside `onSelectSession`.
    var onSelectWorkspaceSession: ((_ workspaceKey: String, _ sessionID: String?) -> Void)?

    private var authorized = false
    private var requested = false
    @Published private(set) var lastTestDiagnostic: DesktopNotificationTestDiagnostic = .idle

    /// Request banner/sound authorization once, early in launch. Safe to call
    /// again; only the first prompts. A denied grant just no-ops later posts.
    func requestAuthorizationIfNeeded() {
        UNUserNotificationCenter.current().delegate = self
        guard !requested else { return }
        requested = true
        UNUserNotificationCenter.current().requestAuthorization(
            options: [.alert, .sound]
        ) { @Sendable [weak self] granted, error in
            let errorDescription = error?.localizedDescription
            Task { @MainActor in
                self?.authorized = granted
                if let errorDescription {
                    NSLog("[UnpeelNative] notification authorization failed: %@", errorDescription)
                }
            }
        }
    }

    /// Verify the pipeline end to end from Settings. Unlike ordinary session
    /// delivery, this reports every outcome back to the Settings row: macOS
    /// acceptance, denied permission, disabled banners, or the concrete
    /// authorization/scheduling error. A unique request id makes every click a
    /// new delivery rather than replacing a prior test in Notification Center.
    func sendTestNotification() {
        let center = UNUserNotificationCenter.current()
        center.delegate = self
        lastTestDiagnostic = .checking
        center.getNotificationSettings { @Sendable [weak self] settings in
            let authorizationStatus = settings.authorizationStatus.rawValue
            let alertsEnabled = settings.alertSetting == .enabled
            Task { @MainActor in
                self?.continueTest(
                    authorizationStatus: authorizationStatus,
                    alertsEnabled: alertsEnabled
                )
            }
        }
    }

    private func continueTest(authorizationStatus: Int, alertsEnabled: Bool) {
        switch authorizationStatus {
        case UNAuthorizationStatus.notDetermined.rawValue:
            requestTestAuthorization()
        case UNAuthorizationStatus.denied.rawValue:
            authorized = false
            lastTestDiagnostic = .denied
        case UNAuthorizationStatus.authorized.rawValue,
             UNAuthorizationStatus.provisional.rawValue:
            authorized = true
            guard alertsEnabled else {
                lastTestDiagnostic = .alertsDisabled
                return
            }
            postTestNotification()
        default:
            authorized = false
            lastTestDiagnostic = .failed("Unknown authorization status")
        }
    }

    private func requestTestAuthorization() {
        UNUserNotificationCenter.current().requestAuthorization(
            options: [.alert, .sound]
        ) { @Sendable [weak self] granted, error in
            let errorDescription = error?.localizedDescription
            Task { @MainActor in
                guard let self else { return }
                if let errorDescription {
                    self.authorized = false
                    self.lastTestDiagnostic = .failed(errorDescription)
                    NSLog(
                        "[UnpeelNative] test notification authorization failed: %@",
                        errorDescription
                    )
                } else if granted {
                    self.authorized = true
                    self.postTestNotification()
                } else {
                    self.authorized = false
                    self.lastTestDiagnostic = .denied
                }
            }
        }
    }

    private func postTestNotification() {
        let content = UNMutableNotificationContent()
        content.title = "Unpeel"
        content.body = "Test notification — this is what a session alert looks like."
        content.sound = .default
        content.threadIdentifier = "unpeel-test"
        content.userInfo = ["kind": "test"]
        let request = UNNotificationRequest(
            identifier: "test:\(UUID().uuidString)",
            content: content,
            trigger: nil
        )
        UNUserNotificationCenter.current().add(request) { @Sendable [weak self] error in
            let errorDescription = error?.localizedDescription
            Task { @MainActor in
                guard let self else { return }
                if let errorDescription {
                    self.lastTestDiagnostic = .failed(errorDescription)
                    NSLog(
                        "[UnpeelNative] test notification scheduling failed: %@",
                        errorDescription
                    )
                } else {
                    self.lastTestDiagnostic = .sent
                }
            }
        }
    }

    /// Post a banner. `sessionID` rides the userInfo so a tap can reselect it;
    /// `kind` matches the phone payload ("needs_input" / "done" / "alert").
    func notify(title: String, body: String, sessionID: String, kind: String) {
        guard authorized else { return }
        let content = UNMutableNotificationContent()
        content.title = title
        content.body = body
        content.sound = .default
        // Collapse repeats for one session into a single banner.
        content.threadIdentifier = sessionID
        content.userInfo = ["sessionID": sessionID, "kind": kind]
        let request = UNNotificationRequest(
            identifier: "\(kind):\(sessionID)",
            content: content,
            trigger: nil
        )
        UNUserNotificationCenter.current().add(request) { error in
            if let error {
                NSLog(
                    "[UnpeelNative] desktop notification scheduling failed: %@",
                    error.localizedDescription
                )
            }
        }
    }

    /// Banner for a session needing input in a BACKGROUND workspace (the
    /// workspace pool's cross-workspace attention). Deduplication happens in
    /// the pool's per-(workspace, session) edge latch; the identifier here
    /// additionally collapses an unclicked stale banner for the same pair.
    func notifyWorkspaceAttention(
        sessionTitle: String,
        workspaceName: String,
        workspaceKey: String,
        sessionID: String
    ) {
        guard authorized else { return }
        let content = UNMutableNotificationContent()
        content.title = sessionTitle
        content.body = "Needs your input — in \(workspaceName)"
        content.sound = .default
        // Collapse repeats per background workspace into one stack.
        content.threadIdentifier = "workspace-attention:\(workspaceKey)"
        content.userInfo = [
            "workspaceKey": workspaceKey,
            "sessionID": sessionID,
            "kind": "workspace_needs_input",
        ]
        let request = UNNotificationRequest(
            identifier: "workspace_needs_input:\(workspaceKey):\(sessionID)",
            content: content,
            trigger: nil
        )
        UNUserNotificationCenter.current().add(request) { error in
            if let error {
                NSLog(
                    "[UnpeelNative] workspace notification scheduling failed: %@",
                    error.localizedDescription
                )
            }
        }
    }

    // Show the banner even while Unpeel is frontmost (the caller already gated
    // on the session not being the observed one, so a banner for a different
    // session is wanted).
    nonisolated func userNotificationCenter(
        _: UNUserNotificationCenter,
        willPresent _: UNNotification,
        withCompletionHandler completionHandler: @escaping (UNNotificationPresentationOptions) -> Void
    ) {
        completionHandler([.banner, .sound])
    }

    nonisolated func userNotificationCenter(
        _: UNUserNotificationCenter,
        didReceive response: UNNotificationResponse,
        withCompletionHandler completionHandler: @escaping () -> Void
    ) {
        let userInfo = response.notification.request.content.userInfo
        let sessionID = userInfo["sessionID"] as? String
        let workspaceKey = userInfo["workspaceKey"] as? String
        // Call back on this (system) queue synchronously; only the store hop
        // needs the main actor. Extracting the Strings first keeps the
        // non-Sendable response off the hop.
        completionHandler()
        if let workspaceKey {
            Task { @MainActor [weak self] in
                self?.onSelectWorkspaceSession?(workspaceKey, sessionID)
            }
        } else if let sessionID {
            Task { @MainActor [weak self] in self?.onSelectSession?(sessionID) }
        }
    }
}
