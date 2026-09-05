//
//  AppLock.swift
//  UnpeelIOS
//
//  Optional Face ID / Touch ID app lock. When armed (Your Mac sheet ▸
//  Security), the app covers itself with AppLockOverlayView on every
//  backgrounding and on cold launch, and clears it only after a successful
//  LocalAuthentication check. Uses .deviceOwnerAuthentication (biometry with
//  device-passcode fallback) so a failed Face ID read can never lock the
//  user out of their own phone app.
//
//  This is a UI shield for shoulder-surfing/handed-phone scenarios — the
//  pairing token already lives in the Keychain; nothing here re-encrypts
//  data at rest.
//

import LocalAuthentication
import SwiftUI

@MainActor
@Observable
final class AppLockManager {
    static let shared = AppLockManager()

    /// Whether the lock is armed. Persisted; change via enable()/disable().
    private(set) var isEnabled: Bool
    /// True while the lock overlay must cover the app.
    private(set) var isLocked: Bool
    /// True while a system auth prompt is up, so the foreground auto-prompt
    /// and the overlay button can never stack two prompts.
    private(set) var authInFlight = false
    /// Last non-cancel auth failure, surfaced on the overlay.
    private(set) var lastError: String?

    /// One automatic prompt per foreground: the Face ID sheet itself bounces
    /// the scene through .inactive → .active, so re-prompting on every
    /// .active would loop after a user cancel.
    private var autoPromptedThisForeground = false

    private static let enabledKey = "unpeel.ios.appLockEnabled"

    private init() {
        let enabled = UserDefaults.standard.bool(forKey: Self.enabledKey)
        isEnabled = enabled
        // Cold launch starts covered when armed.
        isLocked = enabled
    }

    /// Whether the device can authenticate at all (biometry enrolled OR a
    /// passcode set), and which biometry it offers. `biometry == .none` with
    /// `available == true` means passcode-only.
    static func capability() -> (available: Bool, biometry: LABiometryType) {
        let context = LAContext()
        var error: NSError?
        let available = context.canEvaluatePolicy(.deviceOwnerAuthentication, error: &error)
        return (available, context.biometryType)
    }

    /// User-facing name of the unlock method on this device.
    static func methodLabel() -> String {
        let capability = capability()
        switch capability.biometry {
        case .faceID: return "Face ID"
        case .touchID: return "Touch ID"
        case .opticID: return "Optic ID"
        default: return capability.available ? "Passcode" : "Face ID"
        }
    }

    static func methodSymbolName() -> String {
        switch capability().biometry {
        case .faceID: return "faceid"
        case .touchID: return "touchid"
        case .opticID: return "opticid"
        default: return "lock.fill"
        }
    }

    /// Arm the lock. Authenticates first so the toggle confirms the method
    /// actually works before the next backgrounding depends on it.
    func enable() async -> Bool {
        guard !isEnabled else { return true }
        let ok = await authenticate(reason: "Confirm \(Self.methodLabel()) to lock Unpeel when you leave the app")
        guard ok else { return false }
        isEnabled = true
        isLocked = false
        UserDefaults.standard.set(true, forKey: Self.enabledKey)
        return true
    }

    /// Disarm. No auth gate: reaching this toggle already required an unlock.
    func disable() {
        isEnabled = false
        isLocked = false
        UserDefaults.standard.set(false, forKey: Self.enabledKey)
    }

    /// Scene went to .background: cover the app and re-arm the one automatic
    /// foreground prompt.
    func lockIfEnabled() {
        guard isEnabled else { return }
        isLocked = true
        autoPromptedThisForeground = false
    }

    /// Scene became .active: fire the single automatic unlock prompt.
    func autoUnlockOnForeground() async {
        guard isLocked, !autoPromptedThisForeground else { return }
        autoPromptedThisForeground = true
        await unlock()
    }

    /// Clear the lock after a successful auth (overlay button + auto-prompt).
    func unlock() async {
        guard isLocked else { return }
        if await authenticate(reason: "Unlock Unpeel") {
            isLocked = false
        }
    }

    private func authenticate(reason: String) async -> Bool {
        guard !authInFlight else { return false }
        authInFlight = true
        defer { authInFlight = false }
        lastError = nil
        let context = LAContext()
        do {
            return try await context.evaluatePolicy(.deviceOwnerAuthentication, localizedReason: reason)
        } catch let error as LAError
            where error.code == .userCancel || error.code == .systemCancel || error.code == .appCancel {
            // Silent: the overlay's unlock button re-triggers on demand.
            return false
        } catch {
            lastError = error.localizedDescription
            return false
        }
    }
}

/// Full-screen cover shown while the app is locked. Opaque on purpose: it is
/// also what the app-switcher snapshot captures, so session titles and
/// terminal content never appear there while the lock is armed.
struct AppLockOverlayView: View {
    var lock: AppLockManager

    var body: some View {
        ZStack {
            TerminalChrome.background.ignoresSafeArea()
            VStack(spacing: 18) {
                UnpeelBrandLogo(size: 68)
                Text("Unpeel is locked")
                    .font(.system(size: 17, weight: .semibold))
                    .foregroundStyle(.white)
                if let lastError = lock.lastError {
                    Text(lastError)
                        .font(.footnote)
                        .foregroundStyle(.white.opacity(0.55))
                        .multilineTextAlignment(.center)
                        .padding(.horizontal, 32)
                }
                Button {
                    Task { await lock.unlock() }
                } label: {
                    Label(
                        "Unlock with \(AppLockManager.methodLabel())",
                        systemImage: AppLockManager.methodSymbolName()
                    )
                    .font(.subheadline.weight(.semibold))
                    .padding(.horizontal, 18)
                    .padding(.vertical, 12)
                }
                .buttonStyle(.borderedProminent)
                .tint(.cyan)
                .disabled(lock.authInFlight)
                .padding(.top, 6)
            }
        }
        .environment(\.colorScheme, .dark)
    }
}
