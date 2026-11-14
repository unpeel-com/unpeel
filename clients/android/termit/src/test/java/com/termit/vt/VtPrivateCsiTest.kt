package com.termit.vt

import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test

class VtPrivateCsiTest {
    @Test
    fun kittyAndModifyOtherKeysAreNotPrinted() {
        val vt = VtEmulator(40, 4)
        // CSI < u, CSI > 5 u, CSI > 4 ; 2 m — the input-line leak "u5u4;2m".
        val seq = "\u001b[<u\u001b[>5u\u001b[>4;2mhello"
        vt.append(seq.toByteArray(Charsets.UTF_8), seq.length)
        val text = vt.visibleText()
        assertFalse(text.contains("u5u4"))
        assertFalse(text.contains("4;2m"))
        assertTrue(text.contains("hello"))
    }

    @Test
    fun colonSgrIsNotPrinted() {
        val vt = VtEmulator(20, 2)
        val seq = "\u001b[38:2:255:0:0mZ"
        vt.append(seq.toByteArray(Charsets.UTF_8), seq.length)
        val text = vt.visibleText()
        assertFalse(text.contains("255"))
        assertTrue(text.contains("Z"))
    }

    @Test
    fun decPrivateCursorHideStillWorks() {
        val vt = VtEmulator(20, 2)
        val seq = "\u001b[?25l"
        vt.append(seq.toByteArray(Charsets.UTF_8), seq.length)
        assertFalse(vt.shouldCursorBeVisible())
        assertFalse(vt.visibleText().contains("25"))
    }
}
