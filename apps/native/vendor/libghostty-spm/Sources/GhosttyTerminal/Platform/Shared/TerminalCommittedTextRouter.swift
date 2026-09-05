//
//  TerminalCommittedTextRouter.swift
//  libghostty-spm
//

import Foundation

enum TerminalCommittedTextDelivery: Equatable {
    /// Host-managed terminals can forward committed keyboard/IME bytes
    /// directly. They must not go through `ghostty_surface_text`, whose
    /// contract is paste semantics (including bracketed-paste markers).
    case direct(Data)
    /// Exec-backed terminals still need Ghostty to deliver the text.
    case surfaceText(String)
}

enum TerminalCommittedTextRouter {
    static func route(
        _ text: String,
        backend: TerminalSessionBackend
    ) -> TerminalCommittedTextDelivery {
        if case .inMemory = backend {
            return .direct(Data(text.utf8))
        }
        return .surfaceText(text)
    }
}
