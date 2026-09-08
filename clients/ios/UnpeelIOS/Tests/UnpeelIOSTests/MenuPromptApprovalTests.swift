import Foundation
import Testing
@testable import UnpeelIOS

@MainActor
struct MenuPromptApprovalTests {
    // Same approval shape as runtimes/claude-code/fixtures/approval-menu.txt.
    private static let approval = """
        Bash command
        git add -A && git status --short
        Auto mode classifier requires confirmation for this command.

        Do you want to proceed?
        ❯ 1. Yes
          2. Yes, and don't ask again for: git *
          3. No

        Esc to cancel · Tab to amend
        """

    @Test(arguments: ["Tab to amend", "Tab to\n amend"])
    func approvalWithoutNavigationOrConfirmHint(footer: String) {
        let text = Self.approval.replacingOccurrences(of: "Tab to amend", with: footer)
        #expect(RemoteGhosttyRenderer.viewportHasMenuPrompt(text))
        let moved = text.replacingOccurrences(of: "❯ 1.", with: "  1.")
            .replacingOccurrences(of: "  3.", with: "❯ 3.")
        #expect(RemoteGhosttyRenderer.viewportHasMenuPrompt(moved))
    }

    @Test
    func cancelAmendRequiresNearbySelectedChoices() {
        #expect(!RemoteGhosttyRenderer.viewportHasMenuPrompt("Working… Esc to cancel · Tab to amend"))
        #expect(!RemoteGhosttyRenderer.viewportHasMenuPrompt(
            Self.approval.replacingOccurrences(of: "❯", with: " ")
        ))
        #expect(!RemoteGhosttyRenderer.viewportHasMenuPrompt(
            Self.approval.replacingOccurrences(of: "  2.", with: "  x.")
                .replacingOccurrences(of: "  3.", with: "  x.")
        ))
        #expect(!RemoteGhosttyRenderer.viewportHasMenuPrompt(
            Self.approval.replacingOccurrences(of: "Esc to cancel", with:
                String(repeating: "output\n", count: 13) + "Esc to cancel")
        ))
    }
}
