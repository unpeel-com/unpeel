//
//  ComputerPermissions.swift
//  UnpeelNative
//
//  Shared macOS-permission plumbing for the Computer MCP domain. Since the
//  cua-driver refactor the app itself is the TCC grant holder (the embedded
//  daemon is our direct child and inherits our grants), so probing and
//  requesting are native API calls — `AXIsProcessTrusted` /
//  `CGPreflightScreenCaptureAccess` to check, `AXIsProcessTrustedWithOptions`
//  / `CGRequestScreenCaptureAccess` to raise the correctly-attributed system
//  prompts. No engine shell-out; the answers are exactly what the daemon
//  will inherit (after a daemon restart — ComputerEngineManager handles the
//  per-process TCC cache).
//
//  Also home to the /mcp/computer-permissions-needed bridge route — the
//  unified MCP server posts it when an agent's computer action fails because
//  required TCC grants are missing, so the user gets a one-time desktop
//  alert with grant buttons instead of the agent dead-ending on a raw
//  engine error.
//

import AppKit
import ApplicationServices

enum ComputerPermissions {
    struct Row {
        let name: String
        let granted: Bool
        let required: Bool
    }

    /// Engine resolution, same order as computer_mcp.rs resolve_engine_binary:
    /// env override → bundled next to unpeel-host → managed dir → PATH.
    static func resolveEngine() -> String? {
        let fm = FileManager.default
        if let override = ProcessInfo.processInfo.environment["UNPEEL_CUA_DRIVER_BIN"],
           fm.isExecutableFile(atPath: override) {
            return override
        }
        let bundled = (LaunchConfig.hostBinary as NSString)
            .deletingLastPathComponent + "/cua-driver"
        if fm.isExecutableFile(atPath: bundled) {
            return bundled
        }
        let managed = LaunchConfig.unpeelDir
            .appendingPathComponent("computer/bin/cua-driver").path
        if fm.isExecutableFile(atPath: managed) {
            return managed
        }
        let pathDirs = (ProcessInfo.processInfo.environment["PATH"] ?? "")
            .split(separator: ":").map(String.init)
        for dir in pathDirs + ["/opt/homebrew/bin", "/usr/local/bin",
                               NSHomeDirectory() + "/.local/bin"] {
            let candidate = (dir as NSString).appendingPathComponent("cua-driver")
            if fm.isExecutableFile(atPath: candidate) {
                return candidate
            }
        }
        return nil
    }

    static func accessibilityGranted() -> Bool {
        AXIsProcessTrusted()
    }

    static func screenRecordingGranted() -> Bool {
        CGPreflightScreenCaptureAccess()
    }

    /// The two grants computer use needs, probed natively — this process IS
    /// the identity the grants attach to.
    static func probe() -> [Row] {
        [
            Row(name: "Accessibility", granted: accessibilityGranted(), required: true),
            Row(name: "Screen Recording", granted: screenRecordingGranted(), required: true),
        ]
    }

    static func missingRequired() -> [String] {
        probe().filter { $0.required && !$0.granted }.map(\.name)
    }

    /// Raise the system grant prompt for one permission, attributed to
    /// Unpeel.app. First-time calls show the real macOS dialog; once denied,
    /// macOS only registers the app in the pane — so also deep-link there.
    /// Both call sites are SwiftUI button actions, so this hops off the main
    /// actor first: the prompting APIs block their calling thread until the
    /// user answers the system dialog, and a blocked main actor stalls every
    /// queued main-actor job — including the paired-phone bootstrap hop, which
    /// drops remotes to "Connection lost" (same hazard as NSAlert.runModal).
    static func request(_ permission: String) {
        DispatchQueue.global(qos: .userInitiated).async {
            switch permission {
            case "Accessibility":
                let options = ["AXTrustedCheckOptionPrompt": true] as CFDictionary
                _ = AXIsProcessTrustedWithOptions(options)
            case "Screen Recording":
                _ = CGRequestScreenCaptureAccess()
            default:
                break
            }
            DispatchQueue.main.async {
                openPrivacyPane(for: permission)
            }
        }
    }

    static func openPrivacyPane(for permission: String) {
        let anchor: String
        switch permission {
        case "Screen Recording": anchor = "Privacy_ScreenCapture"
        case "Accessibility": anchor = "Privacy_Accessibility"
        default: anchor = "Privacy"
        }
        if let url = URL(
            string: "x-apple.systempreferences:com.apple.preference.security?\(anchor)"
        ) {
            NSWorkspace.shared.open(url)
        }
    }
}

// MARK: - Computer-permissions grant prompt after an approval

extension UnpeelStore {
    func checkComputerPermissionsAfterApproval() {
        let missing = ComputerPermissions.missingRequired()
        guard !missing.isEmpty else { return }
        Task { @MainActor in
            self.presentComputerPermissionsNudge(missing: missing, sessionID: nil)
        }
    }

    /// One prompt per distinct missing-permission set per app run. Granting
    /// one permission changes the set, so a later failure on the remaining
    /// one still prompts. Shown on the shared floating non-modal panel —
    /// this fires from main-actor jobs, where an NSAlert.runModal fallback
    /// would stall every queued main-actor job until answered (including the
    /// paired-phone bootstrap hop).
    func presentComputerPermissionsNudge(missing: [String], sessionID: String?) {
        let key = missing.sorted().joined(separator: "|")
        guard !shownComputerPermissionNudges.contains(key) else { return }
        shownComputerPermissionNudges.insert(key)

        let who = sessionID.map { "“\(sessionDisplayName($0))”" } ?? "An agent"
        computerNudgePanel.show(ComputerPermissionsNudgeView(
            missing: missing,
            subject: who,
            onGrant: { [weak self] permission in
                ComputerPermissions.request(permission)
                self?.computerNudgePanel.dismiss()
            },
            onDismiss: { [weak self] in
                self?.computerNudgePanel.dismiss()
            }
        ))
    }
}
