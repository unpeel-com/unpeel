package com.termit.vt

import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test

class VtMouseTrackingTest {
    @Test
    fun decset1000And1006EnableTrackingAndSgr() {
        val vt = VtEmulator(80, 24)
        val seq = "\u001b[?1000;1006h"
        vt.append(seq.toByteArray(Charsets.UTF_8), seq.length)
        assertTrue(vt.isMouseTrackingActive())
        assertTrue(vt.isMouseProtocolSgr())
    }

    @Test
    fun decrst1000DisablesTracking() {
        val vt = VtEmulator(80, 24)
        val on = "\u001b[?1000;1006h"
        val off = "\u001b[?1000l"
        vt.append(on.toByteArray(Charsets.UTF_8), on.length)
        assertTrue(vt.isMouseTrackingActive())
        vt.append(off.toByteArray(Charsets.UTF_8), off.length)
        assertFalse(vt.isMouseTrackingActive())
    }

    @Test
    fun resetClearsMouseTracking() {
        val vt = VtEmulator(80, 24)
        val on = "\u001b[?1000h"
        vt.append(on.toByteArray(Charsets.UTF_8), on.length)
        assertTrue(vt.isMouseTrackingActive())
        vt.reset()
        assertFalse(vt.isMouseTrackingActive())
    }
}
