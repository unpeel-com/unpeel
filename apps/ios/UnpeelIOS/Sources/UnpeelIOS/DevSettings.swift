//
//  DevSettings.swift
//  UnpeelIOS
//
//  Debug/developer toggles, surfaced in the "Your Mac" sheet's Developer
//  section (DEBUG builds only). Persisted to UserDefaults. Add a flag here +
//  a Toggle in PairingView's developerSection and read it where needed —
//  reading an @Observable property in a view body auto-tracks it, so the UI
//  reacts to the toggle live.
//

import Foundation

@MainActor
@Observable
final class DevSettings {
    static let shared = DevSettings()

    /// Draw a red outline around the terminal grid to inspect its real bounds.
    var showTerminalBounds: Bool {
        didSet { UserDefaults.standard.set(showTerminalBounds, forKey: Self.boundsKey) }
    }

    private static let boundsKey = "unpeel.dev.showTerminalBounds"

    private init() {
        showTerminalBounds = UserDefaults.standard.bool(forKey: Self.boundsKey)
    }
}
