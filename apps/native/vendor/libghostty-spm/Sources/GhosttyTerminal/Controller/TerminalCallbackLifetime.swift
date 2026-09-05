//
//  TerminalCallbackLifetime.swift
//  libghostty-spm
//

import Foundation

/// Stable app-level userdata passed to libghostty.
///
/// Ghostty may issue a wakeup while an app is being torn down. The raw userdata
/// must therefore outlive the Swift controller and only weakly point back to it.
final class TerminalControllerCallbackContext: @unchecked Sendable {
    weak var controller: TerminalController?

    init(controller: TerminalController) {
        self.controller = controller
    }

    func invalidate() {
        controller = nil
    }
}

func releaseRetainedTerminalControllerCallbackContext(address: UInt?) {
    guard let address else { return }
    DispatchQueue.main.async {
        guard let pointer = UnsafeMutableRawPointer(bitPattern: address) else { return }
        Unmanaged<TerminalControllerCallbackContext>.fromOpaque(pointer).release()
    }
}

func releaseRetainedTerminalCallbackBridge(address: UInt?) {
    guard let address else { return }
    DispatchQueue.main.async {
        guard let pointer = UnsafeMutableRawPointer(bitPattern: address) else { return }
        Unmanaged<TerminalCallbackBridge>.fromOpaque(pointer).release()
    }
}
