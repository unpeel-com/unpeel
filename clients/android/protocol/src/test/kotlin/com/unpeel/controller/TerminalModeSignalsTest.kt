package com.unpeel.controller

import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test

class TerminalModeSignalsTest {
    @Test
    fun outputAloneIsNotASnapshot() {
        val signals = TerminalModeSignals()
        signals.feed("hello\r\n".toByteArray())
        assertFalse(signals.hostModeSnapshot)
        assertFalse(signals.sawMouseOrAlternateDisable)
    }

    @Test
    fun mouseOffStopsTheGuess() {
        val signals = TerminalModeSignals()
        signals.feed("\u001b[?1000l".toByteArray())
        assertTrue(signals.sawMouseOrAlternateDisable)
    }

    @Test
    fun mouseOnClearsTheDisable() {
        val signals = TerminalModeSignals()
        signals.feed("\u001b[?1000l\u001b[?1000h\u001b[?1006h".toByteArray())
        assertFalse(signals.sawMouseOrAlternateDisable)
    }

    @Test
    fun resetCountsAsDisable() {
        val signals = TerminalModeSignals()
        signals.feed("\u001bc".toByteArray())
        assertTrue(signals.sawMouseOrAlternateDisable)
    }

    @Test
    fun snapshotSticksUntilReset() {
        val signals = TerminalModeSignals()
        signals.markHostSnapshot()
        signals.feed("\u001b[?1000h\u001b[?1049h".toByteArray())
        assertTrue(signals.hostModeSnapshot)
        signals.reset()
        assertFalse(signals.hostModeSnapshot)
        assertFalse(signals.sawMouseOrAlternateDisable)
    }
}
