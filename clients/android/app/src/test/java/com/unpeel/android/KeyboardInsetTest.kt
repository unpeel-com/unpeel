package com.unpeel.android

import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test

class KeyboardInsetTest {
    @Test
    fun backgroundDropsTheInset() {
        val inset = resolveKeyboardInset(
            allow = false,
            ownsInset = true,
            terminalFocused = true,
            imeTargetPx = 900,
            imePx = 900,
        )
        assertFalse(inset.ownsInset)
        assertEquals(0, inset.padPx)
    }

    @Test
    fun showSnapsToTheTarget() {
        val inset = resolveKeyboardInset(
            allow = true,
            ownsInset = false,
            terminalFocused = true,
            imeTargetPx = 900,
            imePx = 40,
        )
        assertTrue(inset.open)
        assertEquals(900, inset.padPx)
    }

    @Test
    fun hideFollowsTheLiveInset() {
        val inset = resolveKeyboardInset(
            allow = true,
            ownsInset = true,
            terminalFocused = false,
            imeTargetPx = 0,
            imePx = 400,
        )
        assertTrue(inset.ownsInset)
        assertEquals(400, inset.padPx)
    }

    @Test
    fun stuckTargetAfterHideIsIgnored() {
        val inset = resolveKeyboardInset(
            allow = true,
            ownsInset = true,
            terminalFocused = false,
            imeTargetPx = 900,
            imePx = 0,
        )
        assertFalse(inset.open)
        assertEquals(0, inset.padPx)
    }

    @Test
    fun anotherFieldDoesNotShortenTheTerminal() {
        val inset = resolveKeyboardInset(
            allow = true,
            ownsInset = false,
            terminalFocused = false,
            imeTargetPx = 700,
            imePx = 700,
        )
        assertEquals(0, inset.padPx)
    }

    @Test
    fun fullyHiddenReleasesTheLatch() {
        val inset = resolveKeyboardInset(
            allow = true,
            ownsInset = true,
            terminalFocused = false,
            imeTargetPx = 0,
            imePx = 0,
        )
        assertFalse(inset.ownsInset)
        assertEquals(0, inset.padPx)
    }
}
