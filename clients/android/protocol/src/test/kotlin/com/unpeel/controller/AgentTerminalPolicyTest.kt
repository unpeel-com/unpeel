package com.unpeel.controller

import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test

class AgentTerminalPolicyTest {
    @Test
    fun classicClaudeWithSnapshotStaysLocal() {
        assertEquals(
            AgentTerminalPolicy.Wheel.None,
            AgentTerminalPolicy.wheelForwarding(
                modesKnown = true,
                mouseTracking = false,
                alternateScreen = false,
                sawMouseOrAlternateDisable = false,
                prefersRemoteMouseWheel = true,
            ),
        )
    }

    @Test
    fun fullScreenMouseForwardsWheel() {
        assertEquals(
            AgentTerminalPolicy.Wheel.Mouse,
            AgentTerminalPolicy.wheelForwarding(
                modesKnown = true,
                mouseTracking = true,
                alternateScreen = true,
                sawMouseOrAlternateDisable = false,
                prefersRemoteMouseWheel = true,
            ),
        )
    }

    @Test
    fun guessSurvivesUntilModesArrive() {
        assertEquals(
            AgentTerminalPolicy.Wheel.Mouse,
            AgentTerminalPolicy.wheelForwarding(
                modesKnown = false,
                mouseTracking = false,
                alternateScreen = false,
                sawMouseOrAlternateDisable = false,
                prefersRemoteMouseWheel = true,
            ),
        )
        assertEquals(
            AgentTerminalPolicy.Wheel.None,
            AgentTerminalPolicy.wheelForwarding(
                modesKnown = false,
                mouseTracking = false,
                alternateScreen = false,
                sawMouseOrAlternateDisable = true,
                prefersRemoteMouseWheel = true,
            ),
        )
        assertEquals(
            AgentTerminalPolicy.Wheel.None,
            AgentTerminalPolicy.wheelForwarding(
                modesKnown = false,
                mouseTracking = false,
                alternateScreen = false,
                sawMouseOrAlternateDisable = false,
                prefersRemoteMouseWheel = false,
            ),
        )
    }

    @Test
    fun alternateScreenWithoutMouseUsesCursorKeys() {
        assertEquals(
            AgentTerminalPolicy.Wheel.AlternateScroll,
            AgentTerminalPolicy.wheelForwarding(
                modesKnown = true,
                mouseTracking = false,
                alternateScreen = true,
                sawMouseOrAlternateDisable = false,
                prefersRemoteMouseWheel = false,
            ),
        )
    }

    @Test
    fun onlyClaudeGrokAndOpenCodeAreGuessed() {
        assertTrue(AgentTerminalPolicy.prefersRemoteMouseWheel(null, "/bin/zsh", "Claude"))
        assertTrue(AgentTerminalPolicy.prefersRemoteMouseWheel("claude", "/bin/zsh", null))
        assertTrue(AgentTerminalPolicy.prefersRemoteMouseWheel(null, "/opt/homebrew/bin/grok --always-approve", null))
        assertTrue(AgentTerminalPolicy.prefersRemoteMouseWheel(null, "opencode", null))
        assertFalse(AgentTerminalPolicy.prefersRemoteMouseWheel(null, "cursor-agent --force", null))
        assertFalse(AgentTerminalPolicy.prefersRemoteMouseWheel("codex", "codex", null))
        assertFalse(AgentTerminalPolicy.prefersRemoteMouseWheel(null, "/bin/zsh", null))
    }

    @Test
    fun numberedMenuFooterShowsControls() {
        val screen = "❯ 1. Switch\n  2. Keep\n\nEnter to select · ↑/↓ to navigate · Esc to cancel"
        assertTrue(AgentTerminalPolicy.viewportHasMenuPrompt(screen))
    }

    @Test
    fun proseAndPassiveSubagentFooterDoNot() {
        assertFalse(AgentTerminalPolicy.viewportHasMenuPrompt("Use ↑/↓ to navigate, then press Enter to select."))
        assertFalse(
            AgentTerminalPolicy.viewportHasMenuPrompt("  ⏺ main           ↑/↓ to select · Enter to view"),
        )
        val wrapped = "  1. One\n  2. Two\n  ⏺ main           ↑/↓ to select · Enter to\n   view"
        assertFalse(AgentTerminalPolicy.viewportHasMenuPrompt(wrapped))
    }

    @Test
    fun codexConfirmAndAmendQualify() {
        val codex = "  1. Yes, proceed\n  2. No\n\nPress enter to confirm or esc to cancel"
        assertTrue(AgentTerminalPolicy.viewportHasMenuPrompt(codex))
        val amend = "❯ 1. Allow\n  2. Deny\n\nEsc to cancel · Tab to amend"
        assertTrue(AgentTerminalPolicy.viewportHasMenuPrompt(amend))
        assertFalse(AgentTerminalPolicy.viewportHasMenuPrompt("Working… Esc to cancel · Tab to amend"))
    }
}
